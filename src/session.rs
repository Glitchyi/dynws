use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

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
    #[serde(default)]
    pub kind: RepoKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_branch: Option<String>,
}

#[cfg(test)]
impl RepoLink {
    pub fn link(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            kind: RepoKind::Link,
            source_repo: None,
            remote_ref: None,
            local_branch: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    #[default]
    Link,
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSelection {
    pub name: String,
    pub path: PathBuf,
    pub kind: RepoKind,
    pub source_repo: Option<PathBuf>,
    pub remote_ref: Option<String>,
    pub local_branch: Option<String>,
}

impl RepoSelection {
    pub fn link(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            kind: RepoKind::Link,
            source_repo: None,
            remote_ref: None,
            local_branch: None,
        }
    }

    pub fn worktree(
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        source_repo: impl Into<PathBuf>,
        remote_ref: impl Into<String>,
        local_branch: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            kind: RepoKind::Worktree,
            source_repo: Some(source_repo.into()),
            remote_ref: Some(remote_ref.into()),
            local_branch: Some(local_branch.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRemovalOutcome {
    pub session: SessionMetadata,
    pub repo: RepoLink,
    pub removed_worktree: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoAdditionOutcome {
    pub session: SessionMetadata,
    pub repos: Vec<RepoLink>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPatch {
    pub name: Option<String>,
    /// `None` leaves the description unchanged; `Some(None)` clears it.
    pub description: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    Created(SessionMetadata),
    Reused(SessionMetadata),
}

impl CreateOutcome {
    #[cfg(test)]
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
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.paths.sessions_dir)
            .with_context(|| format!("failed to read {}", self.paths.sessions_dir.display()))?
        {
            let entry = entry.context("failed to read session directory entry")?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }

            sessions.push(self.read_session_file(&path)?);
        }

        sessions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(sessions)
    }

    pub fn load_session(&self, name: &str) -> Result<SessionMetadata> {
        let normalized = normalize_name(name)?;
        let path = self.session_file(&normalized);
        self.read_session_file(&path)
            .with_context(|| format!("session '{normalized}' was not found"))
    }

    pub fn rename_session(&self, name: &str, requested_name: &str) -> Result<SessionMetadata> {
        self.edit_session(
            name,
            SessionPatch {
                name: Some(requested_name.to_string()),
                description: None,
            },
        )
    }

    #[cfg(test)]
    pub fn set_session_description(
        &self,
        name: &str,
        description: Option<String>,
    ) -> Result<SessionMetadata> {
        self.edit_session(
            name,
            SessionPatch {
                name: None,
                description: Some(description),
            },
        )
    }

    pub fn edit_session(&self, name: &str, patch: SessionPatch) -> Result<SessionMetadata> {
        let old_name = normalize_name(name)?;
        let new_name = patch
            .name
            .as_deref()
            .map(normalize_name)
            .transpose()?
            .unwrap_or_else(|| old_name.clone());
        let mut metadata = self.load_session(&old_name)?;
        if let Some(description) = patch.description {
            metadata.description = description.and_then(|value| {
                let value = value.trim().to_string();
                (!value.is_empty()).then_some(value)
            });
        }
        metadata.name = new_name.clone();
        metadata.updated_at = current_timestamp();

        if old_name == new_name {
            self.write_session_metadata(&metadata)?;
            return Ok(metadata);
        }

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

        let backup_file = self.paths.sessions_dir.join(format!(
            ".{old_name}.{}.{}.bak",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        fs::rename(&old_file, &backup_file)
            .with_context(|| format!("failed to stage {}", old_file.display()))?;
        let workspace_moved = fs::symlink_metadata(&old_workspace).is_ok();
        if workspace_moved {
            if let Err(error) = fs::rename(&old_workspace, &new_workspace) {
                let _ = fs::rename(&backup_file, &old_file);
                return Err(error).with_context(|| {
                    format!(
                        "failed to rename workspace {} -> {}",
                        old_workspace.display(),
                        new_workspace.display()
                    )
                });
            }
        }

        if let Err(error) = self.write_session_metadata(&metadata) {
            if workspace_moved {
                let _ = fs::rename(&new_workspace, &old_workspace);
            }
            let _ = fs::rename(&backup_file, &old_file);
            return Err(error);
        }
        let _ = fs::remove_file(&backup_file);
        Ok(metadata)
    }

    pub fn remove_session(&self, name: &str) -> Result<SessionMetadata> {
        let normalized = normalize_name(name)?;
        let metadata = self.load_session(&normalized)?;
        let workspace = self.workspace_path(&normalized);
        let file = self.session_file(&normalized);
        validate_workspace(&workspace, &metadata)?;

        let nonce = format!("{}.{}", std::process::id(), Utc::now().timestamp_micros());
        let staged_file = self
            .paths
            .sessions_dir
            .join(format!(".{normalized}.{nonce}.remove"));
        let staged_workspace = self
            .paths
            .workspaces_dir
            .join(format!(".{normalized}.{nonce}.remove"));
        fs::rename(&file, &staged_file)
            .with_context(|| format!("failed to stage {}", file.display()))?;

        let workspace_exists = fs::symlink_metadata(&workspace).is_ok();
        if workspace_exists {
            if let Err(error) = fs::rename(&workspace, &staged_workspace) {
                let _ = fs::rename(&staged_file, &file);
                return Err(error)
                    .with_context(|| format!("failed to stage {}", workspace.display()));
            }
        }

        if workspace_exists {
            if let Err(error) = fs::remove_dir_all(&staged_workspace) {
                let _ = fs::rename(&staged_workspace, &workspace);
                let _ = fs::rename(&staged_file, &file);
                return Err(error)
                    .with_context(|| format!("failed to remove {}", staged_workspace.display()));
            }
        }
        let _ = fs::remove_file(&staged_file);
        Ok(metadata)
    }

    pub fn remove_repo_link(
        &self,
        session_name: &str,
        repo_name: &str,
    ) -> Result<RepoRemovalOutcome> {
        validate_link_name(repo_name)?;
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
        let dws_worktree = matches!(repo.kind, RepoKind::Worktree)
            && self.is_dws_worktree_path(&repo_path)
            && managed_worktree_identity_matches(&repo).unwrap_or(false);
        let link_path = self.workspace_path(&normalized).join(&repo.name);
        let staged_link = self.workspace_path(&normalized).join(format!(
            ".{}.{}.{}.remove",
            repo.name,
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        let link_exists = match fs::symlink_metadata(&link_path) {
            Ok(link_metadata) if link_metadata.file_type().is_symlink() => true,
            Ok(_) => bail!(
                "workspace repo entry is not a symlink; refusing removal: {}",
                link_path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", link_path.display()));
            }
        };
        if link_exists {
            fs::rename(&link_path, &staged_link)
                .with_context(|| format!("failed to stage {}", link_path.display()))?;
        }
        metadata.repos.remove(repo_idx);
        metadata.updated_at = current_timestamp();
        if let Err(error) = self.write_session_metadata(&metadata) {
            if link_exists {
                let _ = fs::rename(&staged_link, &link_path);
            }
            return Err(error);
        }
        if link_exists {
            let _ = fs::remove_file(&staged_link);
        }

        let shared = dws_worktree
            && self
                .load_sessions()?
                .iter()
                .flat_map(|session| &session.repos)
                .any(|other| {
                    PathBuf::from(&other.path).canonicalize().ok() == repo_path.canonicalize().ok()
                });
        let (removed_worktree, warning) = if dws_worktree && !shared {
            match git::remove_worktree(&repo_path) {
                Ok(()) => (true, None),
                Err(error) => (
                    false,
                    Some(format!(
                        "repo link removed, but managed worktree was preserved at {}: {error:#}",
                        repo_path.display()
                    )),
                ),
            }
        } else if dws_worktree {
            (
                false,
                Some("managed worktree is still referenced by another session".to_string()),
            )
        } else if linked_worktree || matches!(repo.kind, RepoKind::Worktree) {
            (
                false,
                Some(format!(
                    "worktree ownership could not be verified; preserved {} and removed the session link only",
                    repo_path.display()
                )),
            )
        } else {
            (false, None)
        };

        Ok(RepoRemovalOutcome {
            session: metadata,
            repo,
            removed_worktree,
            warning,
        })
    }

    pub fn add_repo_links(
        &self,
        session_name: &str,
        repos: &[RepoSelection],
    ) -> Result<RepoAdditionOutcome> {
        let normalized = normalize_name(session_name)?;
        let mut metadata = self.load_session(&normalized)?;
        let canonical_repos = canonical_selection_set(repos, &self.paths)?;
        let workspace = self.workspace_path(&normalized);

        let mut link_names = metadata
            .repos
            .iter()
            .map(|repo| repo.name.clone())
            .collect::<HashSet<_>>();
        let mut repo_paths = metadata
            .repos
            .iter()
            .filter_map(|repo| PathBuf::from(&repo.path).canonicalize().ok())
            .collect::<HashSet<_>>();

        for repo in &canonical_repos {
            if !link_names.insert(repo.name.clone()) {
                bail!(
                    "repo link '{}' already exists in session '{}'",
                    repo.name,
                    normalized
                );
            }
            if !repo_paths.insert(repo.path.clone()) {
                bail!(
                    "repo path is already linked in session '{}': {}",
                    normalized,
                    repo.path.display()
                );
            }

            let link_path = workspace.join(&repo.name);
            match fs::symlink_metadata(&link_path) {
                Ok(_) => bail!(
                    "workspace link path already exists: {}",
                    link_path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", link_path.display()));
                }
            }
        }

        match fs::symlink_metadata(&workspace) {
            Ok(workspace_metadata)
                if !workspace_metadata.file_type().is_dir()
                    || workspace_metadata.file_type().is_symlink() =>
            {
                bail!("workspace path is not a directory: {}", workspace.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&workspace)
                    .with_context(|| format!("failed to create {}", workspace.display()))?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", workspace.display()));
            }
        }

        let mut added = Vec::with_capacity(canonical_repos.len());
        let mut created_links = Vec::with_capacity(canonical_repos.len());
        for repo in &canonical_repos {
            let link_path = workspace.join(&repo.name);
            if let Err(error) = create_symlink(&repo.path, &link_path).with_context(|| {
                format!(
                    "failed to link {} -> {}",
                    link_path.display(),
                    repo.path.display()
                )
            }) {
                rollback_created_links(&created_links);
                return Err(error);
            }
            created_links.push(link_path);
            added.push(repo_link_from_selection(repo));
        }

        metadata.repos.extend(added.clone());
        metadata
            .repos
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        metadata.updated_at = current_timestamp();
        if let Err(error) = self.write_session_metadata(&metadata) {
            rollback_created_links(&created_links);
            return Err(error);
        }

        Ok(RepoAdditionOutcome {
            session: metadata,
            repos: added,
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

    #[cfg(test)]
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
        let canonical_repos = canonical_selection_set(repos, &self.paths)?;
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
        let session_file = self.session_file(&name);
        if session_file.exists() {
            let existing = self.load_session(&name)?;
            if metadata_repo_set(&existing).as_ref() == Some(&repo_paths) {
                return Ok(CreateOutcome::Reused(existing));
            }
            bail!("session '{name}' already exists with a different repo set");
        }

        let workspace = self.workspace_path(&name);
        if fs::symlink_metadata(&workspace).is_ok() {
            bail!("workspace target already exists: {}", workspace.display());
        }
        let staged_workspace = self.paths.workspaces_dir.join(format!(
            ".{name}.{}.{}.create",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        fs::create_dir(&staged_workspace)
            .with_context(|| format!("failed to create {}", staged_workspace.display()))?;

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

            let link_path = staged_workspace.join(&repo.name);
            if let Err(error) = create_symlink(&repo.path, &link_path) {
                let _ = fs::remove_dir_all(&staged_workspace);
                return Err(error).with_context(|| {
                    format!(
                        "failed to link {} -> {}",
                        link_path.display(),
                        repo.path.display()
                    )
                });
            }
            repo_links.push(repo_link_from_selection(repo));
        }

        let timestamp = Utc::now().to_rfc3339();
        let metadata = SessionMetadata {
            name,
            description,
            repos: repo_links,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        if let Err(error) = self.write_session_metadata(&metadata) {
            let _ = fs::remove_dir_all(&staged_workspace);
            return Err(error);
        }
        if let Err(error) = fs::rename(&staged_workspace, &workspace) {
            let _ = fs::remove_file(self.session_file(&metadata.name));
            let _ = fs::remove_dir_all(&staged_workspace);
            return Err(error)
                .with_context(|| format!("failed to publish {}", workspace.display()));
        }

        Ok(CreateOutcome::Created(metadata))
    }

    fn write_session_metadata(&self, metadata: &SessionMetadata) -> Result<()> {
        let name = normalize_name(&metadata.name)?;
        validate_session_metadata(metadata, &name, &self.paths)?;
        let session_file = self.session_file(&name);
        let temp_file = self.paths.sessions_dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            Utc::now().timestamp_micros()
        ));
        let data = toml::to_string_pretty(metadata).context("failed to serialize session")?;
        fs::write(&temp_file, data)
            .with_context(|| format!("failed to write {}", temp_file.display()))?;
        if let Err(error) = fs::rename(&temp_file, &session_file) {
            let _ = fs::remove_file(&temp_file);
            return Err(error).with_context(|| {
                format!(
                    "failed to replace session metadata {}",
                    session_file.display()
                )
            });
        }
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

    fn read_session_file(&self, path: &Path) -> Result<SessionMetadata> {
        let data = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let session: SessionMetadata =
            toml::from_str(&data).with_context(|| format!("failed to parse {}", path.display()))?;
        let expected_name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .context("session file has no valid UTF-8 stem")?;
        validate_session_metadata(&session, expected_name, &self.paths)?;
        Ok(session)
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

#[cfg(test)]
fn selections_from_paths(repos: &[PathBuf]) -> Result<Vec<RepoSelection>> {
    repos
        .iter()
        .map(|repo| {
            let name = repo
                .file_name()
                .and_then(|value| value.to_str())
                .context("repo path has no valid terminal component")?
                .to_string();
            Ok(RepoSelection::link(name, repo.clone()))
        })
        .collect()
}

fn validate_link_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    let is_single_component = matches!(
        path.components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    );
    if name.trim().is_empty() || name != name.trim() || !is_single_component {
        bail!("repo link name is not safe to use as a path component: {name}");
    }
    Ok(())
}

fn validate_session_metadata(
    session: &SessionMetadata,
    expected_name: &str,
    paths: &DynwsPaths,
) -> Result<()> {
    let normalized = normalize_name(&session.name)?;
    if session.name != normalized || session.name != expected_name {
        bail!(
            "session metadata name '{}' does not match file name '{}'",
            session.name,
            expected_name
        );
    }

    let mut names = HashSet::new();
    let mut repo_paths = HashSet::new();
    for repo in &session.repos {
        validate_link_name(&repo.name)?;
        if !names.insert(repo.name.as_str()) {
            bail!(
                "session '{}' contains duplicate repo link '{}'",
                session.name,
                repo.name
            );
        }

        let repo_path = PathBuf::from(&repo.path);
        if !repo_path.is_absolute() {
            bail!("repo path must be absolute: {}", repo.path);
        }
        let identity = repo_path.canonicalize().unwrap_or(repo_path.clone());
        if !repo_paths.insert(identity) {
            bail!(
                "session '{}' contains duplicate repo path {}",
                session.name,
                repo.path
            );
        }

        match repo.kind {
            RepoKind::Link => {
                if repo.source_repo.is_some()
                    || repo.remote_ref.is_some()
                    || repo.local_branch.is_some()
                {
                    bail!(
                        "normal repo link '{}' contains worktree metadata",
                        repo.name
                    );
                }
            }
            RepoKind::Worktree => {
                let source_repo = repo
                    .source_repo
                    .as_deref()
                    .context("managed worktree is missing source_repo")?;
                if !Path::new(source_repo).is_absolute() {
                    bail!("managed worktree source must be absolute: {source_repo}");
                }
                if repo.remote_ref.as_deref().is_none_or(str::is_empty)
                    || repo.local_branch.as_deref().is_none_or(str::is_empty)
                {
                    bail!(
                        "managed worktree '{}' is missing branch metadata",
                        repo.name
                    );
                }
                if !path_is_within(&repo_path, &paths.worktrees_dir) {
                    bail!(
                        "managed worktree path is outside dws storage: {}",
                        repo_path.display()
                    );
                }
                if repo_path.exists() && !managed_worktree_identity_matches(repo)? {
                    bail!(
                        "managed worktree identity does not match its recorded source and branch: {}",
                        repo_path.display()
                    );
                }
            }
        }
    }

    Ok(())
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => path.is_absolute() && root.is_absolute() && path.starts_with(root),
    }
}

fn managed_worktree_identity_matches(repo: &RepoLink) -> Result<bool> {
    let Some(source_repo) = repo.source_repo.as_deref() else {
        return Ok(false);
    };
    let Some(local_branch) = repo.local_branch.as_deref() else {
        return Ok(false);
    };
    let Some(info) = git::inspect_worktree(Path::new(&repo.path))? else {
        return Ok(false);
    };
    let source_repo = Path::new(source_repo)
        .canonicalize()
        .with_context(|| format!("failed to canonicalize worktree source {source_repo}"))?;
    Ok(info.source_repo == source_repo && info.local_branch.as_deref() == Some(local_branch))
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

fn canonical_selection_set(repos: &[RepoSelection], paths: &DynwsPaths) -> Result<Vec<RepoSelection>> {
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
        let (source_repo, remote_ref, local_branch) = match repo.kind {
            RepoKind::Link => {
                if repo.source_repo.is_some()
                    || repo.remote_ref.is_some()
                    || repo.local_branch.is_some()
                {
                    bail!(
                        "normal repo selection '{}' contains worktree metadata",
                        repo.name
                    );
                }
                (None, None, None)
            }
            RepoKind::Worktree => {
                let source_repo = repo
                    .source_repo
                    .as_ref()
                    .context("worktree selection is missing source repo")?
                    .canonicalize()
                    .context("failed to canonicalize worktree source repo")?;
                if !path.join(".git").is_file() {
                    bail!(
                        "worktree selection is not a linked git worktree: {}",
                        path.display()
                    );
                }
                let remote_ref = repo
                    .remote_ref
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .context("worktree selection is missing remote ref")?
                    .clone();
                let local_branch = repo
                    .local_branch
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .context("worktree selection is missing local branch")?
                    .clone();
                if !path_is_within(&path, &paths.worktrees_dir) {
                    bail!("worktree selection is outside dws storage: {}", path.display());
                }
                let info = git::inspect_worktree(&path)?
                    .with_context(|| format!("{} is not a linked git worktree", path.display()))?;
                if info.source_repo != source_repo
                    || info.local_branch.as_deref() != Some(local_branch.as_str())
                {
                    bail!(
                        "worktree selection identity does not match its source repository and branch: {}",
                        path.display()
                    );
                }
                (Some(source_repo), Some(remote_ref), Some(local_branch))
            }
        };
        canonical.push(RepoSelection {
            name: repo.name.clone(),
            path,
            kind: repo.kind,
            source_repo,
            remote_ref,
            local_branch,
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

fn repo_link_from_selection(repo: &RepoSelection) -> RepoLink {
    RepoLink {
        name: repo.name.clone(),
        path: display_path(&repo.path),
        kind: repo.kind,
        source_repo: repo.source_repo.as_deref().map(display_path),
        remote_ref: repo.remote_ref.clone(),
        local_branch: repo.local_branch.clone(),
    }
}

fn current_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn validate_workspace(workspace: &Path, session: &SessionMetadata) -> Result<()> {
    let metadata = match fs::symlink_metadata(workspace) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", workspace.display()));
        }
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "workspace path is not a managed directory: {}",
            workspace.display()
        );
    }

    let expected = session
        .repos
        .iter()
        .map(|repo| repo.name.as_str())
        .collect::<HashSet<_>>();
    for entry in fs::read_dir(workspace)
        .with_context(|| format!("failed to read {}", workspace.display()))?
    {
        let entry = entry.context("failed to read workspace entry")?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .context("workspace contains a non-UTF-8 entry")?;
        let entry_metadata = fs::symlink_metadata(entry.path())
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if !expected.contains(name) || !entry_metadata.file_type().is_symlink() {
            bail!(
                "workspace contains an unmanaged entry; refusing removal: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn rollback_created_links(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
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
                &[RepoSelection::link("repo", worktree_path.clone())],
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
    fn adds_repo_links_as_an_ordered_batch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let beta = make_repo(&repos, "beta");
        let gamma = make_repo(&repos, "gamma");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        let created = store
            .create_session(
                "demo",
                std::slice::from_ref(&alpha),
                Some("keep this description".to_string()),
                false,
            )
            .unwrap()
            .into_metadata();

        let outcome = store
            .add_repo_links(
                "demo",
                &[
                    RepoSelection::link("gamma", gamma.clone()),
                    RepoSelection::link("beta", beta.clone()),
                ],
            )
            .unwrap();

        assert_eq!(
            outcome
                .session
                .repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );
        assert_eq!(
            outcome
                .repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta", "gamma"]
        );
        assert_eq!(outcome.session.created_at, created.created_at);
        assert_eq!(
            outcome.session.description.as_deref(),
            Some("keep this description")
        );
        assert_eq!(
            fs::read_link(home.join("workspaces/demo/beta")).unwrap(),
            beta.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_link(home.join("workspaces/demo/gamma")).unwrap(),
            gamma.canonicalize().unwrap()
        );
        assert!(alpha.exists());
        assert!(beta.exists());
        assert!(gamma.exists());
    }

    #[test]
    fn rejects_duplicate_repo_links_without_mutating_the_session() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let beta = make_repo(&repos, "beta");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", std::slice::from_ref(&alpha), None, false)
            .unwrap();

        let duplicate_name = store
            .add_repo_links("demo", &[RepoSelection::link("alpha", beta.clone())])
            .unwrap_err();
        assert!(duplicate_name.to_string().contains("already exists"));

        let duplicate_path = store
            .add_repo_links("demo", &[RepoSelection::link("beta", alpha.clone())])
            .unwrap_err();
        assert!(duplicate_path.to_string().contains("already linked"));

        let loaded = store.load_session("demo").unwrap();
        assert_eq!(loaded.repos.len(), 1);
        assert!(!home.join("workspaces/demo/beta").exists());
        assert!(alpha.exists());
        assert!(beta.exists());
    }

    #[test]
    fn validates_the_full_add_batch_before_creating_links() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repos = temp.path().join("repos");
        let alpha = make_repo(&repos, "alpha");
        let beta = make_repo(&repos, "beta");
        let gamma = make_repo(&repos, "gamma");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", std::slice::from_ref(&alpha), None, false)
            .unwrap();
        fs::write(home.join("workspaces/demo/gamma"), "occupied").unwrap();

        let error = store
            .add_repo_links(
                "demo",
                &[
                    RepoSelection::link("beta", beta),
                    RepoSelection::link("gamma", gamma),
                ],
            )
            .unwrap_err();

        assert!(error.to_string().contains("already exists"));
        assert!(!home.join("workspaces/demo/beta").exists());
        assert_eq!(store.load_session("demo").unwrap().repos.len(), 1);
        assert!(store.add_repo_links("demo", &[]).is_err());
        assert!(
            store
                .add_repo_links(
                    "demo",
                    &[RepoSelection::link("missing", temp.path().join("missing"))],
                )
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rolls_back_added_links_when_metadata_cannot_be_persisted() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let alpha = make_repo(&temp.path().join("repos"), "alpha");
        let beta = make_repo(&temp.path().join("repos"), "beta");
        let store = SessionStore::new(DynwsPaths::from_home(&home));
        store
            .create_session("demo", std::slice::from_ref(&alpha), None, false)
            .unwrap();
        let sessions_dir = home.join("sessions");
        let original_permissions = fs::metadata(&sessions_dir).unwrap().permissions();
        let mut read_only = original_permissions.clone();
        read_only.set_mode(0o555);
        fs::set_permissions(&sessions_dir, read_only).unwrap();

        let result = store.add_repo_links("demo", &[RepoSelection::link("beta", beta)]);

        fs::set_permissions(&sessions_dir, original_permissions).unwrap();
        assert!(result.is_err());
        assert!(!home.join("workspaces/demo/beta").exists());
        assert_eq!(store.load_session("demo").unwrap().repos.len(), 1);
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
        let paths = DynwsPaths::from_home(&home);
        let worktree = git::create_worktree(&repo, &paths.worktrees_dir, "Feature A").unwrap();
        let store = SessionStore::new(paths);
        store
            .create_session_with_links(
                "demo",
                &[RepoSelection::worktree(
                    "repo",
                    worktree.clone(),
                    repo.clone(),
                    "HEAD",
                    "feature-a",
                )],
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
