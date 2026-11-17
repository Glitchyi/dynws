//! The shared filesystem commit and owned-resource rollback for session creation.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::DynwsPaths;
use crate::git::{self, OriginBranch, StrictBranch};
use crate::session::{RepoLink, RepoSelection, SessionMetadata};

enum OwnedWorktree {
    Strict {
        link: RepoLink,
        branch: StrictBranch,
        complete: bool,
    },
    Interactive(RepoLink),
}

#[derive(Default)]
pub(crate) struct Resources {
    owned: Vec<OwnedWorktree>,
}

impl Resources {
    pub fn run<T>(operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let mut resources = Self::default();
        match operation(&mut resources) {
            Ok(result) => Ok(result),
            Err(error) => {
                let warnings = resources.rollback();
                bail!(
                    "{error:#}{}",
                    if warnings.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", warnings.join("\n"))
                    }
                );
            }
        }
    }

    pub fn strict(&mut self, link: &RepoLink, branch: &StrictBranch) -> Result<()> {
        fs::create_dir(&link.path).context("worktree target reservation failed")?;
        self.owned.push(OwnedWorktree::Strict {
            link: link.clone(),
            branch: branch.clone(),
            complete: false,
        });
        git::strict_add(
            Path::new(link.source_repo.as_deref().context("missing source")?),
            Path::new(&link.path),
            branch,
        )?;
        if let Some(OwnedWorktree::Strict { complete, .. }) = self.owned.last_mut() {
            *complete = true;
        }
        validate_strict_identity(link)
    }

    pub fn interactive(
        &mut self,
        name: &str,
        source: &Path,
        paths: &DynwsPaths,
        branch: &OriginBranch,
    ) -> Result<RepoSelection> {
        let outcome = git::ensure_worktree_from_origin(source, &paths.worktrees_dir, branch)?;
        let selection = RepoSelection::worktree(
            name,
            outcome.path,
            outcome.source_repo,
            outcome.remote_ref,
            outcome.local_branch,
        );
        if outcome.created {
            self.owned.push(OwnedWorktree::Interactive(RepoLink {
                name: selection.name.clone(),
                path: selection.path.to_string_lossy().into_owned(),
                kind: selection.kind,
                source_repo: selection
                    .source_repo
                    .as_deref()
                    .map(|p| p.to_string_lossy().into_owned()),
                remote_ref: selection.remote_ref.clone(),
                local_branch: selection.local_branch.clone(),
            }));
        }
        Ok(selection)
    }

    pub fn rollback(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        for owned in self.owned.drain(..).rev() {
            let (link, strict, complete, new_branch) = match owned {
                OwnedWorktree::Strict {
                    link,
                    branch,
                    complete,
                } => (link, true, complete, branch.create),
                OwnedWorktree::Interactive(link) => (link, false, true, false),
            };
            let path = Path::new(&link.path);
            if exists(path).unwrap_or(true) {
                let cleanup = if complete {
                    validate_identity(&link, strict).and_then(|()| git::remove_worktree(path))
                } else {
                    fs::remove_dir(path).context(
                        "creation did not complete; retained nonempty target for inspection",
                    )
                };
                if let Err(error) = cleanup {
                    warnings.push(format!("retained worktree {}: {error:#}", path.display()));
                }
            }
            if new_branch {
                warnings.push(format!("branch '{}' may have been created and is retained in {}; retry only after inspecting it, or explicitly select it with branch", link.local_branch.as_deref().unwrap_or_default(), link.source_repo.as_deref().unwrap_or_default()));
            }
        }
        warnings
    }
}

pub(crate) fn validate_strict_identity(link: &RepoLink) -> Result<()> {
    validate_identity(link, true)
}

fn validate_identity(link: &RepoLink, upstream: bool) -> Result<()> {
    let path = Path::new(&link.path);
    anyhow::ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "managed target is a symlink"
    );
    let info =
        git::inspect_worktree(path)?.context("target is not a registered linked worktree")?;
    anyhow::ensure!(
        info.source_repo.to_str() == link.source_repo.as_deref()
            && info.local_branch == link.local_branch
            && (!upstream || link.remote_ref.is_none() || info.upstream == link.remote_ref),
        "managed worktree source, branch or upstream identity changed: {}",
        path.display()
    );
    Ok(())
}

pub(crate) fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn cleanup_links(path: &Path, metadata: &SessionMetadata, warnings: &mut Vec<String>) {
    for repo in &metadata.repos {
        let link = path.join(&repo.name);
        if fs::read_link(&link).ok().as_deref() == Some(Path::new(&repo.path))
            && let Err(error) = fs::remove_file(&link)
        {
            warnings.push(format!("retained link {}: {error}", link.display()));
        }
    }
    if let Err(error) = fs::remove_dir(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warnings.push(format!(
            "retained staging/workspace directory {}: {error}",
            path.display()
        ));
    }
}

/// Metadata publication is the commit point. Never replace foreign state.
pub(crate) fn publish(
    paths: &DynwsPaths,
    metadata: &SessionMetadata,
    prepare: impl FnOnce() -> Result<()>,
) -> Result<Vec<String>> {
    let name = &metadata.name;
    let workspace = paths.workspaces_dir.join(name);
    let session_file = paths.sessions_dir.join(format!("{name}.toml"));
    let nonce = format!(
        "{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let stage = paths.workspaces_dir.join(format!(".{name}.{nonce}.create"));
    let staged_metadata = paths.sessions_dir.join(format!(".{name}.{nonce}.create"));
    let serialized = toml::to_string_pretty(metadata)?;
    let mut stage_created = false;
    let mut metadata_staged = false;
    let mut workspace_published = false;
    let operation = (|| -> Result<()> {
        fs::create_dir(&stage)?;
        stage_created = true;
        prepare()?;
        for repo in &metadata.repos {
            std::os::unix::fs::symlink(&repo.path, stage.join(&repo.name))?;
        }
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_metadata)?;
        metadata_staged = true;
        staged.write_all(serialized.as_bytes())?;
        staged.sync_all()?;
        anyhow::ensure!(
            !exists(&session_file)? && !exists(&workspace)?,
            "workspace publication collision"
        );
        fs::create_dir(&workspace)?;
        workspace_published = true;
        fs::rename(&stage, &workspace)?;
        fs::hard_link(&staged_metadata, &session_file)?;
        Ok(())
    })();
    if let Err(error) = operation {
        let mut warnings = Vec::new();
        if workspace_published {
            cleanup_links(&workspace, metadata, &mut warnings);
        }
        if stage_created {
            cleanup_links(&stage, metadata, &mut warnings);
        }
        if metadata_staged && let Err(error) = fs::remove_file(&staged_metadata) {
            warnings.push(format!(
                "retained metadata staging file {}: {error}",
                staged_metadata.display()
            ));
        }
        bail!(
            "{error:#}{}",
            if warnings.is_empty() {
                String::new()
            } else {
                format!("\n{}", warnings.join("\n"))
            }
        );
    }
    let mut warnings = Vec::new();
    if let Err(error) = fs::remove_file(&staged_metadata) {
        warnings.push(format!(
            "retained staging file {}: {error}",
            staged_metadata.display()
        ));
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_preserves_foreign_metadata_and_unexpected_staging_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DynwsPaths::initialize_at(temp.path().join(".dynws"), temp.path()).unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let metadata = SessionMetadata {
            creation: None,
            name: "collision".into(),
            description: None,
            repos: vec![RepoLink::link("source", source.to_string_lossy())],
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let foreign = paths.sessions_dir.join("collision.toml");
        let error = publish(&paths, &metadata, || {
            fs::write(&foreign, "keep metadata")?;
            let stage = fs::read_dir(&paths.workspaces_dir)?.next().unwrap()?.path();
            fs::write(stage.join("unexpected"), "keep file")?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("collision"));
        assert_eq!(fs::read_to_string(foreign).unwrap(), "keep metadata");
        assert!(!paths.workspaces_dir.join("collision").exists());
        let stage = fs::read_dir(&paths.workspaces_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read_to_string(stage.join("unexpected")).unwrap(),
            "keep file"
        );
        assert!(!stage.join("source").exists());
        assert_eq!(fs::read_dir(&paths.sessions_dir).unwrap().count(), 1);
    }
}
