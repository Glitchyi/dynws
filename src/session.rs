use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::{DynwsPaths, display_path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub repos: Vec<RepoLink>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoLink {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    Created(SessionMetadata),
    Reused(SessionMetadata),
}

impl CreateOutcome {
    pub fn metadata(&self) -> &SessionMetadata {
        match self {
            Self::Created(metadata) | Self::Reused(metadata) => metadata,
        }
    }

    pub fn into_metadata(self) -> SessionMetadata {
        match self {
            Self::Created(metadata) | Self::Reused(metadata) => metadata,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    paths: DynwsPaths,
}

impl SessionStore {
    pub fn new(paths: DynwsPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &DynwsPaths {
        &self.paths
    }

    pub fn workspace_path(&self, session: &str) -> PathBuf {
        self.paths.workspaces_dir.join(session)
    }

    pub fn load_sessions(&self) -> Result<Vec<SessionMetadata>> {
        if !self.paths.sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.paths.sessions_dir)
            .with_context(|| format!("failed to read {}", self.paths.sessions_dir.display()))?
        {
            let entry = entry.context("failed to read session directory entry")?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }

            let data = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let session: SessionMetadata = toml::from_str(&data)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            sessions.push(session);
        }

        sessions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(sessions)
    }

    pub fn load_session(&self, name: &str) -> Result<SessionMetadata> {
        let normalized = normalize_name(name)?;
        let path = self.session_file(&normalized);
        let data = fs::read_to_string(&path)
            .with_context(|| format!("session '{normalized}' was not found"))?;
        let session =
            toml::from_str(&data).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(session)
    }

    pub fn find_duplicate(&self, repos: &[PathBuf]) -> Result<Option<SessionMetadata>> {
        let selected = canonical_repo_set(repos)?;

        for session in self.load_sessions()? {
            let Some(existing) = metadata_repo_set(&session) else {
                continue;
            };

            if existing == selected {
                return Ok(Some(session));
            }
        }

        Ok(None)
    }

    pub fn create_session(
        &self,
        requested_name: &str,
        repos: &[PathBuf],
        description: Option<String>,
        allow_duplicate: bool,
    ) -> Result<CreateOutcome> {
        let canonical_repos = canonical_repo_set(repos)?;

        if !allow_duplicate {
            if let Some(existing) = self.find_duplicate(repos)? {
                return Ok(CreateOutcome::Reused(existing));
            }
        }

        let name = normalize_name(requested_name)?;
        self.paths.ensure_layout()?;

        let session_file = self.session_file(&name);
        if session_file.exists() {
            let existing = self.load_session(&name)?;
            if metadata_repo_set(&existing).as_ref() == Some(&canonical_repos) {
                return Ok(CreateOutcome::Reused(existing));
            }
            bail!("session '{name}' already exists with a different repo set");
        }

        let workspace = self.workspace_path(&name);
        if workspace.exists() && !workspace.is_dir() {
            bail!("workspace path is not a directory: {}", workspace.display());
        }
        fs::create_dir_all(&workspace)
            .with_context(|| format!("failed to create {}", workspace.display()))?;

        let mut link_names = HashSet::new();
        let mut repo_links = Vec::new();
        for repo in &canonical_repos {
            let repo_name = repo
                .file_name()
                .and_then(|value| value.to_str())
                .context("repo path has no valid terminal component")?
                .to_string();
            if !link_names.insert(repo_name.clone()) {
                bail!("multiple selected repos would create the same link name '{repo_name}'");
            }

            let link_path = workspace.join(&repo_name);
            create_or_validate_symlink(repo, &link_path)?;
            repo_links.push(RepoLink {
                name: repo_name,
                path: display_path(repo),
            });
        }

        let timestamp = Utc::now().to_rfc3339();
        let metadata = SessionMetadata {
            name,
            description,
            repos: repo_links,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        let data = toml::to_string_pretty(&metadata).context("failed to serialize session")?;
        fs::write(&session_file, data)
            .with_context(|| format!("failed to write {}", session_file.display()))?;

        Ok(CreateOutcome::Created(metadata))
    }

    fn session_file(&self, name: &str) -> PathBuf {
        self.paths.sessions_dir.join(format!("{name}.toml"))
    }
}

pub fn normalize_name(input: &str) -> Result<String> {
    let mut output = String::new();
    let mut previous_dash = false;

    for character in input.trim().chars() {
        let next = if character.is_ascii_alphanumeric() || character == '_' || character == '.' {
            previous_dash = false;
            character.to_ascii_lowercase()
        } else {
            if previous_dash {
                continue;
            }
            previous_dash = true;
            '-'
        };
        output.push(next);
    }

    let output = output.trim_matches(['-', '.']).to_string();
    if output.is_empty() {
        bail!("session name cannot be empty");
    }
    if output == "." || output == ".." || output.contains('/') {
        bail!("session name is not safe to use as a path component");
    }

    Ok(output)
}

fn canonical_repo_set(repos: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if repos.is_empty() {
        bail!("at least one repo must be selected");
    }

    let mut canonical = Vec::with_capacity(repos.len());
    for repo in repos {
        let path = repo
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", repo.display()))?;
        if !path.is_dir() {
            bail!("repo path is not a directory: {}", path.display());
        }
        canonical.push(path);
    }

    canonical.sort();
    canonical.dedup();
    Ok(canonical)
}

fn metadata_repo_set(session: &SessionMetadata) -> Option<Vec<PathBuf>> {
    let mut repos = Vec::with_capacity(session.repos.len());
    for repo in &session.repos {
        let path = PathBuf::from(&repo.path).canonicalize().ok()?;
        repos.push(path);
    }
    repos.sort();
    repos.dedup();
    Some(repos)
}

fn create_or_validate_symlink(target: &Path, link: &Path) -> Result<()> {
    if fs::symlink_metadata(link).is_ok() {
        let existing = fs::read_link(link)
            .with_context(|| format!("failed to read existing link {}", link.display()))?;
        let existing = if existing.is_absolute() {
            existing
        } else {
            link.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(existing)
        };
        let existing = existing.canonicalize().unwrap_or(existing);
        if existing == target {
            return Ok(());
        }
        bail!(
            "link path already exists and points elsewhere: {}",
            link.display()
        );
    }

    create_symlink(target, link)
        .with_context(|| format!("failed to link {} -> {}", link.display(), target.display()))
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> Result<()> {
    bail!("dynws currently supports native symlinks on Linux/macOS only")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_repo(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn normalizes_names_for_paths() {
        assert_eq!(normalize_name("My Session").unwrap(), "my-session");
        assert_eq!(normalize_name("  Feature/API  ").unwrap(), "feature-api");
        assert!(normalize_name("...").is_err());
    }

    #[test]
    fn creates_workspace_symlinks_and_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let beta = make_repo(&repos, "beta");
        let store = SessionStore::new(DynwsPaths::from_home(&home));

        let outcome = store
            .create_session("My Session", &[alpha.clone(), beta.clone()], None, false)
            .unwrap();

        assert!(matches!(outcome, CreateOutcome::Created(_)));
        let metadata = outcome.metadata();
        assert_eq!(metadata.name, "my-session");
        assert!(home.join("sessions/my-session.toml").exists());
        assert_eq!(
            fs::read_link(home.join("workspaces/my-session/alpha")).unwrap(),
            alpha.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_link(home.join("workspaces/my-session/beta")).unwrap(),
            beta.canonicalize().unwrap()
        );
    }

    #[test]
    fn reuses_duplicate_repo_sets() {
        let temp = tempfile::tempdir().unwrap();
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let beta = make_repo(&repos, "beta");
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join("home")));

        let first = store
            .create_session("one", &[alpha.clone(), beta.clone()], None, false)
            .unwrap();
        let second = store
            .create_session("two", &[beta, alpha], None, false)
            .unwrap();

        assert!(matches!(first, CreateOutcome::Created(_)));
        assert!(matches!(second, CreateOutcome::Reused(_)));
        assert_eq!(second.metadata().name, "one");
    }

    #[test]
    fn allows_explicit_duplicates_with_different_names() {
        let temp = tempfile::tempdir().unwrap();
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join("home")));

        store
            .create_session("one", std::slice::from_ref(&alpha), None, false)
            .unwrap();
        let second = store
            .create_session("two", std::slice::from_ref(&alpha), None, true)
            .unwrap();

        assert!(matches!(second, CreateOutcome::Created(_)));
        assert_eq!(store.load_sessions().unwrap().len(), 2);
    }
}
