use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::DynwsPaths;
use crate::git::{self, StrictBranch};
use crate::manifest::{self, CreationRecord, InputFormat};
use crate::publication::{exists, validate_strict_identity as validate_identity};
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
    let timestamp = chrono::Utc::now().to_rfc3339();
    let metadata = SessionMetadata {
        creation: Some(record),
        name,
        description: None,
        repos: plans.iter().map(|plan| plan.link.clone()).collect(),
        created_at: timestamp.clone(),
        updated_at: timestamp,
    };
    // Prepare output before any writes, preserving the strict manifest contract.
    let mut success = result(paths, &metadata, "created")?;
    let worktrees = plans
        .iter()
        .filter_map(|plan| {
            plan.branch
                .as_ref()
                .map(|branch| (plan.link.clone(), branch.clone()))
        })
        .collect::<Vec<_>>();
    success.warnings = store.publish_strict_session(&metadata, &worktrees)?;
    Ok(success)
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("workspace paths must be valid UTF-8 for JSON/TOML metadata")
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
