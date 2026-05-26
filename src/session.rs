use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::{DynwsPaths, display_path};
use crate::git;

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
pub struct RepoSelection {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRemovalOutcome {
    pub session: SessionMetadata,
    pub repo: RepoLink,
    pub removed_worktree: bool,
    pub warning: Option<String>,
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

    pub fn rename_session(&self, name: &str, requested_name: &str) -> Result<SessionMetadata> {
        let old_name = normalize_name(name)?;
        let new_name = normalize_name(requested_name)?;
        let mut metadata = self.load_session(&old_name)?;
        if old_name == new_name {
            return Ok(metadata);
        }

        self.paths.ensure_layout()?;
        let old_file = self.session_file(&old_name);
        let new_file = self.session_file(&new_name);
        if new_file.exists() {
            bail!("session '{new_name}' already exists");
        }

        let old_workspace = self.workspace_path(&old_name);
        let new_workspace = self.workspace_path(&new_name);
        if fs::symlink_metadata(&new_workspace).is_ok() {
            bail!(
                "workspace target already exists: {}",
                new_workspace.display()
            );
        }

        metadata.name = new_name.clone();
        metadata.updated_at = current_timestamp();
        self.write_session_metadata(&metadata)?;

        if fs::symlink_metadata(&old_workspace).is_ok() {
            if let Err(error) = fs::rename(&old_workspace, &new_workspace) {
                let _ = fs::remove_file(&new_file);
                bail!(
                    "failed to rename workspace {} -> {}: {error}",
                    old_workspace.display(),
                    new_workspace.display()
                );
            }
        }

        fs::remove_file(&old_file)
            .with_context(|| format!("failed to remove {}", old_file.display()))?;
        Ok(metadata)
    }

    pub fn set_session_description(
        &self,
        name: &str,
        description: Option<String>,
    ) -> Result<SessionMetadata> {
        let mut metadata = self.load_session(name)?;
        metadata.description = description.and_then(|value| {
            let value = value.trim().to_string();
            (!value.is_empty()).then_some(value)
        });
        metadata.updated_at = current_timestamp();
        self.write_session_metadata(&metadata)?;
        Ok(metadata)
    }

    pub fn remove_session(&self, name: &str) -> Result<SessionMetadata> {
        let normalized = normalize_name(name)?;
        let metadata = self.load_session(&normalized)?;
        let workspace = self.workspace_path(&normalized);
        remove_workspace_path(&workspace)?;

        let file = self.session_file(&normalized);
        fs::remove_file(&file).with_context(|| format!("failed to remove {}", file.display()))?;
        Ok(metadata)
    }

    pub fn remove_repo_link(
        &self,
        session_name: &str,
        repo_name: &str,
    ) -> Result<RepoRemovalOutcome> {
        let normalized = normalize_name(session_name)?;
        let mut metadata = self.load_session(&normalized)?;
        let repo_idx = metadata
            .repos
            .iter()
            .position(|repo| repo.name == repo_name)
            .with_context(|| {
                format!("repo link '{repo_name}' was not found in session '{normalized}'")
            })?;
        let repo = metadata.repos[repo_idx].clone();
        let repo_path = PathBuf::from(&repo.path);
        let linked_worktree = git::is_linked_worktree(&repo_path).unwrap_or(false);
        let dws_worktree = linked_worktree && self.is_dws_worktree_path(&repo_path);

        if dws_worktree {
            git::remove_worktree(&repo_path)
                .with_context(|| format!("failed to remove worktree {}", repo_path.display()))?;
        }

        let link_path = self.workspace_path(&normalized).join(&repo.name);
        remove_workspace_path(&link_path)?;
        metadata.repos.remove(repo_idx);
        metadata.updated_at = current_timestamp();
        self.write_session_metadata(&metadata)?;

        let warning = if linked_worktree && !dws_worktree {
            Some(
                "repo link appears to be a worktree outside dws storage; removed session link only"
                    .to_string(),
            )
        } else {
            None
        };

        Ok(RepoRemovalOutcome {
            session: metadata,
            repo,
            removed_worktree: dws_worktree,
            warning,
        })
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
        let selections = selections_from_paths(repos)?;
        self.create_session_with_links(requested_name, &selections, description, allow_duplicate)
    }

    pub fn create_session_with_links(
        &self,
        requested_name: &str,
        repos: &[RepoSelection],
        description: Option<String>,
        allow_duplicate: bool,
    ) -> Result<CreateOutcome> {
        let canonical_repos = canonical_selection_set(repos)?;
        let repo_paths = canonical_repos
            .iter()
            .map(|repo| repo.path.clone())
            .collect::<Vec<_>>();

        if !allow_duplicate {
            if let Some(existing) = self.find_duplicate(&repo_paths)? {
                return Ok(CreateOutcome::Reused(existing));
            }
        }

        let name = normalize_name(requested_name)?;
        self.paths.ensure_layout()?;

        let session_file = self.session_file(&name);
        if session_file.exists() {
            let existing = self.load_session(&name)?;
            if metadata_repo_set(&existing).as_ref() == Some(&repo_paths) {
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
            validate_link_name(&repo.name)?;
            if !link_names.insert(repo.name.clone()) {
                bail!(
                    "multiple selected repos would create the same link name '{}'",
                    repo.name
                );
            }

            let link_path = workspace.join(&repo.name);
            create_or_validate_symlink(&repo.path, &link_path)?;
            repo_links.push(RepoLink {
                name: repo.name.clone(),
                path: display_path(&repo.path),
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
        self.write_session_metadata(&metadata)?;

        Ok(CreateOutcome::Created(metadata))
    }

    fn write_session_metadata(&self, metadata: &SessionMetadata) -> Result<()> {
        self.paths.ensure_layout()?;
        let name = normalize_name(&metadata.name)?;
        let session_file = self.session_file(&name);
        let data = toml::to_string_pretty(metadata).context("failed to serialize session")?;
        fs::write(&session_file, data)
            .with_context(|| format!("failed to write {}", session_file.display()))?;
        Ok(())
    }

    fn is_dws_worktree_path(&self, path: &Path) -> bool {
        let Ok(path) = path.canonicalize() else {
            return false;
        };
        let Ok(worktrees_dir) = self.paths.worktrees_dir.canonicalize() else {
            return false;
        };
        path.starts_with(worktrees_dir)
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

fn selections_from_paths(repos: &[PathBuf]) -> Result<Vec<RepoSelection>> {
    repos
        .iter()
        .map(|repo| {
            let name = repo
                .file_name()
                .and_then(|value| value.to_str())
                .context("repo path has no valid terminal component")?
                .to_string();
            Ok(RepoSelection {
                name,
                path: repo.clone(),
            })
        })
        .collect()
}

fn validate_link_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.contains('/') || name == "." || name == ".." {
        bail!("repo link name is not safe to use as a path component: {name}");
    }
    Ok(())
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

fn canonical_selection_set(repos: &[RepoSelection]) -> Result<Vec<RepoSelection>> {
    if repos.is_empty() {
        bail!("at least one repo must be selected");
    }

    let mut canonical = Vec::with_capacity(repos.len());
    for repo in repos {
        validate_link_name(&repo.name)?;
        let path = repo
            .path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", repo.path.display()))?;
        if !path.is_dir() {
            bail!("repo path is not a directory: {}", path.display());
        }
        canonical.push(RepoSelection {
            name: repo.name.clone(),
            path,
        });
    }

    canonical.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
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

fn current_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn remove_workspace_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }

    Ok(())
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
    use std::process::Command;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn git_output_text(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git command failed: {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

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

    #[test]
    fn supports_custom_link_names_for_worktree_paths() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let worktree_path = make_repo(&temp.path().join("worktrees/repo"), "main");
        let store = SessionStore::new(DynwsPaths::from_home(&home));

        let outcome = store
            .create_session_with_links(
                "worktree",
                &[RepoSelection {
                    name: "repo".to_string(),
                    path: worktree_path.clone(),
                }],
                None,
                false,
            )
            .unwrap();

        assert!(matches!(outcome, CreateOutcome::Created(_)));
        assert_eq!(
            fs::read_link(home.join("workspaces/worktree/repo")).unwrap(),
            worktree_path.canonicalize().unwrap()
        );
        assert_eq!(outcome.metadata().repos[0].name, "repo");
    }

    #[test]
    fn renames_session_metadata_and_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = make_repo(&temp.path().join("repos"), "alpha");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("old name", std::slice::from_ref(&repo), None, false)
            .unwrap();

        let renamed = store.rename_session("old-name", "New Name").unwrap();

        assert_eq!(renamed.name, "new-name");
        assert!(!home.join("sessions/old-name.toml").exists());
        assert!(home.join("sessions/new-name.toml").exists());
        assert!(!home.join("workspaces/old-name").exists());
        assert_eq!(
            fs::read_link(home.join("workspaces/new-name/alpha")).unwrap(),
            repo.canonicalize().unwrap()
        );
    }

    #[test]
    fn updates_session_description() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = make_repo(&temp.path().join("repos"), "alpha");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", std::slice::from_ref(&repo), None, false)
            .unwrap();

        let updated = store
            .set_session_description("demo", Some("  important workspace  ".to_string()))
            .unwrap();
        let cleared = store.set_session_description("demo", None).unwrap();

        assert_eq!(updated.description.as_deref(), Some("important workspace"));
        assert_eq!(cleared.description, None);
    }

    #[test]
    fn removes_session_without_removing_linked_repos() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = make_repo(&temp.path().join("repos"), "alpha");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", std::slice::from_ref(&repo), None, false)
            .unwrap();

        let removed = store.remove_session("demo").unwrap();

        assert_eq!(removed.name, "demo");
        assert!(!home.join("sessions/demo.toml").exists());
        assert!(!home.join("workspaces/demo").exists());
        assert!(repo.exists());
    }

    #[test]
    fn removes_individual_repo_link_without_removing_repo() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let beta = make_repo(&repos, "beta");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", &[alpha.clone(), beta.clone()], None, false)
            .unwrap();

        let outcome = store.remove_repo_link("demo", "alpha").unwrap();

        assert_eq!(outcome.repo.name, "alpha");
        assert!(!outcome.removed_worktree);
        assert_eq!(
            outcome
                .session
                .repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta"]
        );
        assert!(!home.join("workspaces/demo/alpha").exists());
        assert!(alpha.exists());
        assert!(beta.exists());
    }

    #[test]
    fn removing_last_repo_link_leaves_empty_session() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let alpha = make_repo(&temp.path().join("repos"), "alpha");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", std::slice::from_ref(&alpha), None, false)
            .unwrap();

        let outcome = store.remove_repo_link("demo", "alpha").unwrap();

        assert!(outcome.session.repos.is_empty());
        assert!(home.join("sessions/demo.toml").exists());
        assert!(!home.join("workspaces/demo/alpha").exists());
        assert!(alpha.exists());
    }

    #[test]
    fn removing_dws_worktree_repo_link_removes_git_worktree() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("README.md"), "hello").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(
            &repo,
            &[
                "-c",
                "user.name=dynws",
                "-c",
                "user.email=dynws@example.invalid",
                "commit",
                "-m",
                "init",
            ],
        );
        let worktree = git::create_worktree(&repo, &home.join("worktrees"), "Feature A").unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session_with_links(
                "demo",
                &[RepoSelection {
                    name: "repo".to_string(),
                    path: worktree.clone(),
                }],
                None,
                false,
            )
            .unwrap();

        let outcome = store.remove_repo_link("demo", "repo").unwrap();
        let worktrees = git_output_text(&repo, &["worktree", "list"]);

        assert!(outcome.removed_worktree);
        assert!(!worktree.exists());
        assert!(!worktrees.contains(&worktree.display().to_string()));
    }
}
