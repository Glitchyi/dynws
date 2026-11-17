use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::DynwsPaths;
use crate::git::{self, StrictBranch};
use crate::manifest::{self, CreationRecord, InputFormat};
use crate::session::{RepoKind, RepoLink, SessionMetadata, SessionStore};

#[derive(Debug, Serialize)]
pub struct CreateResult {
    schema_version: u32,
    name: String,
    pub path: PathBuf,
    status: &'static str,
    repos: Vec<ResultRepo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ResultRepo {
    name: String,
    input: PathBuf,
    source: PathBuf,
    kind: RepoKind,
    path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_commit: Option<String>,
}

struct Plan {
    link: RepoLink,
    branch: Option<StrictBranch>,
}

pub fn create(
    paths: &DynwsPaths,
    file: &Path,
    format: Option<InputFormat>,
    cwd: &Path,
    fetch: bool,
    reuse: bool,
) -> Result<CreateResult> {
    let (manifest, base) = manifest::load(file, format, cwd)?;
    let (name, mut record) = manifest::normalize(manifest, &base)?;
    let store = SessionStore::new(paths.clone()).locked()?;
    let mut fetched = HashSet::new();
    for repo in &mut record.repos {
        if repo.kind == RepoKind::Worktree {
            let source = git::strict_source(&repo.input)?;
            if fetch && fetched.insert(source.clone()) {
                git::strict_git(&source, &["fetch", "origin"])?;
            }
            repo.source = Some(source);
        }
    }
    let session_file = paths.sessions_dir.join(format!("{name}.toml"));
    if exists(&session_file)? {
        if !reuse {
            bail!("workspace '{name}' already exists; use --reuse for an exact match");
        }
        let existing = store.load_session(&name)?;
        validate_reuse(paths, &existing, &record)?;
        return result(paths, &existing, "reused");
    }
    if exists(&store.workspace_path(&name))? {
        bail!("workspace target already exists without matching published metadata");
    }
    let mut plans = Vec::new();
    let mut targets = HashSet::new();
    let mut branches = HashSet::new();
    for repo in &mut record.repos {
        let (path, branch) = if let Some(source) = &repo.source {
            let branch = match &repo.branch {
                Some(expression) => git::strict_branch(source, expression)?,
                None => git::strict_new_branch(
                    source,
                    repo.new_branch.as_deref().context("missing new branch")?,
                    repo.base.as_deref().context("missing base")?,
                )?,
            };
            git::strict_available(source, &branch.local)?;
            if !branches.insert((source.clone(), branch.local.clone())) {
                bail!(
                    "multiple inputs select the same source branch '{}'; use one worktree",
                    branch.local
                );
            }
            if repo.new_branch.is_some() {
                repo.initial_commit = Some(branch.start.clone());
            }
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            source.hash(&mut hash);
            branch.local.hash(&mut hash);
            let path = paths
                .worktrees_dir
                .join(format!("strict-{:016x}", hash.finish()));
            if exists(&path)? {
                bail!(
                    "managed target {} already exists; only a published matching workspace may reuse it",
                    path.display()
                );
            }
            (path, Some(branch))
        } else {
            (repo.input.clone(), None)
        };
        if !targets.insert(path.clone()) {
            bail!("duplicate target path {}", path.display());
        }
        plans.push(Plan {
            link: RepoLink {
                name: repo.name.clone(),
                path: utf8(&path)?,
                kind: repo.kind,
                source_repo: repo.source.as_deref().map(utf8).transpose()?,
                remote_ref: branch.as_ref().and_then(|b| b.remote.clone()),
                local_branch: branch.as_ref().map(|b| b.local.clone()),
            },
            branch,
        });
    }
    let workspace = store.workspace_path(&name);
    let nonce = format!(
        "{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let stage = paths.workspaces_dir.join(format!(".{name}.{nonce}.create"));
    let staged_metadata = paths.sessions_dir.join(format!(".{name}.{nonce}.create"));
    let timestamp = chrono::Utc::now().to_rfc3339();
    let metadata = SessionMetadata {
        creation: Some(record),
        name,
        description: None,
        repos: plans.iter().map(|p| p.link.clone()).collect(),
        created_at: timestamp.clone(),
        updated_at: timestamp,
    };
    let serialized = toml::to_string_pretty(&metadata)?;
    // Prepare the complete result before any mutations, including UTF-8/serialization checks.
    let success = result(paths, &metadata, "created")?;
    let mut attempted = Vec::new();
    let mut workspace_published = false;
    let mut stage_created = false;
    let mut metadata_staged = false;
    let operation = (|| -> Result<()> {
        fs::create_dir(&stage)?;
        stage_created = true;
        for plan in &plans {
            if let Some(branch) = &plan.branch {
                // Reserve an absent target so Git cannot adopt a directory that appeared
                // after preflight. Git accepts an existing empty worktree directory.
                fs::create_dir(&plan.link.path).context("worktree target reservation failed")?;
                attempted.push((plan, false));
                git::strict_add(
                    Path::new(plan.link.source_repo.as_deref().context("missing source")?),
                    Path::new(&plan.link.path),
                    branch,
                )?;
                attempted.last_mut().expect("tracked worktree attempt").1 = true;
                validate_identity(&plan.link)?;
            }
            std::os::unix::fs::symlink(&plan.link.path, stage.join(&plan.link.name))?;
        }
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_metadata)?;
        metadata_staged = true;
        use std::io::Write;
        staged.write_all(serialized.as_bytes())?;
        staged.sync_all()?;
        if exists(&session_file)? || exists(&workspace)? {
            bail!("workspace publication collision");
        }
        // mkdir reserves the final name without replacing any existing directory.
        fs::create_dir(&workspace)?;
        workspace_published = true;
        // Replace only our empty reservation. Rename refuses a directory containing
        // unexpected entries and publishes all links together.
        fs::rename(&stage, &workspace)?;
        // Hard-link publication is atomic and refuses to overwrite existing metadata.
        fs::hard_link(&staged_metadata, &session_file)?;
        Ok(())
    })();
    if let Err(error) = operation {
        let mut retained = Vec::new();
        if workspace_published {
            cleanup_links(&workspace, &metadata.repos, &mut retained);
        }
        if stage_created {
            cleanup_links(&stage, &metadata.repos, &mut retained);
        }
        for (plan, created) in attempted.into_iter().rev() {
            let path = Path::new(&plan.link.path);
            if exists(path).unwrap_or(true) {
                let cleanup = if created {
                    validate_identity(&plan.link).and_then(|()| git::remove_worktree(path))
                } else {
                    fs::remove_dir(path).context(
                        "creation did not complete; retained nonempty target for inspection",
                    )
                };
                if let Err(e) = cleanup {
                    retained.push(format!("retained worktree {}: {e:#}", path.display()));
                }
            }
            if plan.branch.as_ref().is_some_and(|b| b.create) {
                retained.push(format!("branch '{}' may have been created and is retained in {}; retry only after inspecting it, or explicitly select it with branch", plan.link.local_branch.as_deref().unwrap_or_default(), plan.link.source_repo.as_deref().unwrap_or_default()));
            }
        }
        if let Err(e) = if metadata_staged {
            remove_file_if_present(&staged_metadata)
        } else {
            Ok(())
        } {
            retained.push(format!(
                "retained metadata staging file {}: {e}",
                staged_metadata.display()
            ));
        }
        bail!(
            "{error:#}{}",
            if retained.is_empty() {
                String::new()
            } else {
                format!("\n{}", retained.join("\n"))
            }
        );
    }
    // The committed workspace is valid even if its extra staging name cannot be removed.
    let mut success = success;
    if let Err(e) = fs::remove_file(&staged_metadata) {
        success.warnings.push(format!(
            "retained staging file {}: {e}",
            staged_metadata.display()
        ));
    }
    drop(store);
    Ok(success)
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("workspace paths must be valid UTF-8 for JSON/TOML metadata")
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn cleanup_links(path: &Path, repos: &[RepoLink], warnings: &mut Vec<String>) {
    for repo in repos {
        let link = path.join(&repo.name);
        if fs::read_link(&link).ok().as_deref() == Some(Path::new(&repo.path))
            && let Err(e) = fs::remove_file(&link)
        {
            warnings.push(format!("retained link {}: {e}", link.display()));
        }
    }
    if let Err(e) = fs::remove_dir(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warnings.push(format!(
            "retained staging/workspace directory {}: {e}",
            path.display()
        ));
    }
}

fn validate_identity(repo: &RepoLink) -> Result<()> {
    let path = Path::new(&repo.path);
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("managed target is a symlink");
    }
    let info =
        git::inspect_worktree(path)?.context("target is not a registered linked worktree")?;
    if Some(utf8(&info.source_repo)?) != repo.source_repo
        || info.local_branch != repo.local_branch
        || (repo.remote_ref.is_some() && info.upstream != repo.remote_ref)
    {
        bail!(
            "managed worktree source, branch or upstream identity changed: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_reuse(
    paths: &DynwsPaths,
    session: &SessionMetadata,
    requested: &CreationRecord,
) -> Result<()> {
    let recorded = session
        .creation
        .as_ref()
        .context("legacy workspace lacks creation provenance; strict reuse is unavailable")?;
    if recorded.schema_version != 1 {
        bail!("unsupported creation provenance version");
    }
    let mut old = recorded.clone();
    for repo in &mut old.repos {
        repo.initial_commit = None;
    }
    if &old != requested {
        bail!(
            "workspace creation intent does not match; paths, aliases, modes and branch intent must match exactly"
        );
    }
    let workspace = paths.workspaces_dir.join(&session.name);
    let meta = fs::symlink_metadata(&workspace).context("workspace directory is missing")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        bail!("workspace must be a real directory");
    }
    let entries = fs::read_dir(&workspace)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() != session.repos.len() || session.repos.len() != recorded.repos.len() {
        bail!("workspace links or metadata repository set changed");
    }
    let links: HashMap<_, _> = session.repos.iter().map(|r| (r.name.as_str(), r)).collect();
    for repo in &recorded.repos {
        let link = links
            .get(repo.name.as_str())
            .context("recorded repository missing from session")?;
        if link.kind != repo.kind
            || fs::read_link(workspace.join(&repo.name))
                .context("workspace link missing or altered")?
                != Path::new(&link.path)
        {
            bail!("workspace symlink or mode changed for '{}'", repo.name);
        }
        if repo.kind == RepoKind::Link {
            if Path::new(&link.path).canonicalize()? != repo.input {
                bail!("link target changed");
            }
        } else {
            if link.source_repo.as_deref().map(Path::new) != repo.source.as_deref() {
                bail!("recorded source changed");
            }
            validate_identity(link)?;
            let source = repo.source.as_deref().context("missing source")?;
            if let Some(expression) = &repo.branch {
                let branch = git::strict_branch(source, expression)?;
                if link.local_branch.as_deref() != Some(&branch.local)
                    || link.remote_ref != branch.remote
                {
                    bail!("branch intent differs from actual worktree");
                }
            } else if link.local_branch != repo.new_branch || repo.initial_commit.is_none() {
                bail!("new-branch provenance is incomplete or changed");
            }
        }
    }
    Ok(())
}

fn result(
    paths: &DynwsPaths,
    session: &SessionMetadata,
    status: &'static str,
) -> Result<CreateResult> {
    let record = session
        .creation
        .as_ref()
        .context("missing creation provenance")?;
    let mut repos = Vec::new();
    for repo in &record.repos {
        let link = session
            .repos
            .iter()
            .find(|r| r.name == repo.name)
            .context("missing repository metadata")?;
        repos.push(ResultRepo {
            name: repo.name.clone(),
            input: repo.input.clone(),
            source: repo.source.clone().unwrap_or_else(|| repo.input.clone()),
            kind: repo.kind,
            path: PathBuf::from(&link.path),
            branch: link.local_branch.clone(),
            base: repo.base.clone(),
            initial_commit: repo.initial_commit.clone(),
        });
    }
    Ok(CreateResult {
        schema_version: 1,
        name: session.name.clone(),
        path: paths.workspaces_dir.join(&session.name),
        status,
        repos,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_worktrees_support_conservative_session_cleanup() {
        for dirty in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let source = root.join("api");
            fs::create_dir(&source).unwrap();
            git::strict_git(&source, &["init", "-b", "main"]).unwrap();
            git::strict_git(
                &source,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ],
            )
            .unwrap();
            let paths = DynwsPaths::initialize_at(root.join(".dynws"), &root).unwrap();
            let file = root.join("workspace.toml");
            fs::write(&file, "schema_version=1\nname='cleanup'\n[[repos]]\npath='api'\nkind='worktree'\nnew_branch='topic'\nbase='main'\n").unwrap();
            let created = create(&paths, &file, None, &root, false, false).unwrap();
            let target = &created.repos[0].path;
            if dirty {
                fs::write(target.join("dirty"), "keep").unwrap();
            }
            let store = SessionStore::new(paths);
            assert!(
                store.load_session("cleanup").unwrap().repos[0]
                    .remote_ref
                    .is_none()
            );
            let removal = store.remove_repo_link("cleanup", "api").unwrap();
            assert_eq!(removal.removed_worktree, !dirty);
            assert_eq!(target.exists(), dirty);
            assert_eq!(
                git::strict_git(
                    &source,
                    &[
                        "for-each-ref",
                        "--format=%(refname:short)",
                        "refs/heads/topic"
                    ]
                )
                .unwrap(),
                "topic"
            );
        }
    }
}
