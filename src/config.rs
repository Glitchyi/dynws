use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const PROJECT_SCHEMA_VERSION: u32 = 1;
const PROJECT_FILE_NAME: &str = "project.toml";

#[derive(Debug, Clone)]
pub struct DynwsPaths {
    pub home: PathBuf,
    pub collection_root: PathBuf,
    pub project_file: PathBuf,
    pub config_file: PathBuf,
    pub sessions_dir: PathBuf,
    pub workspaces_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProjectMarker {
    schema_version: u32,
    collection_root: PathBuf,
}

impl DynwsPaths {
    /// Resolve the initialized project that contains the current directory.
    ///
    /// Unlike initialization, resolution never creates or repairs files.
    pub fn resolve_from(cwd: &Path) -> Result<Self> {
        let cwd = canonical_directory(cwd, "current directory")?;
        if let Some(home) = dynws_home_override(&cwd)? {
            return Self::load_initialized(&home).with_context(|| {
                format!(
                    "DYNWS_HOME does not point to an initialized dws project: {}",
                    home.display()
                )
            });
        }

        for ancestor in cwd.ancestors() {
            let home = ancestor.join(".dynws");
            let project_file = home.join(PROJECT_FILE_NAME);
            if path_entry_exists(&project_file)? {
                let paths = Self::load_initialized(&home)?;
                if paths.collection_root != ancestor {
                    bail!(
                        "project marker {} records collection root {}, but its project root is {}",
                        project_file.display(),
                        paths.collection_root.display(),
                        ancestor.display()
                    );
                }
                return Ok(paths);
            }
        }

        bail!(
            "no initialized dws project found from {}; run 'dws init' at the repository collection root",
            cwd.display()
        )
    }

    /// Initialize the project selected by the CLI environment.
    ///
    /// With `DYNWS_HOME`, the override is the exact storage directory. Without
    /// it, storage is created in `<cwd>/.dynws`.
    pub fn initialize_from(cwd: &Path) -> Result<Self> {
        let collection_root = canonical_directory(cwd, "collection root")?;
        if let Some(home) = dynws_home_override(&collection_root)? {
            if path_entry_exists(&home.join(PROJECT_FILE_NAME))? {
                return Self::repair_initialized(&home);
            }
            return Self::initialize_at(home, &collection_root);
        }

        Self::initialize_at(collection_root.join(".dynws"), &collection_root)
    }

    /// Initialize a specific storage directory. This is also useful for
    /// isolated internal tests which must not depend on process environment.
    pub fn initialize_at(home: impl Into<PathBuf>, collection_root: &Path) -> Result<Self> {
        let collection_root = canonical_directory(collection_root, "collection root")?;
        let home = absolute_path(home.into(), &collection_root);
        let project_file = home.join(PROJECT_FILE_NAME);

        if path_entry_exists(&project_file)? {
            let marker = read_project_marker(&project_file)?;
            let recorded_root = validate_marker(&marker, &project_file)?;
            if recorded_root != collection_root {
                bail!(
                    "{} belongs to collection root {}, not {}",
                    project_file.display(),
                    recorded_root.display(),
                    collection_root.display()
                );
            }
            return Self::repair_initialized(&home);
        }

        validate_legacy_layout(&home)?;
        create_layout(&home)?;

        let marker = ProjectMarker {
            schema_version: PROJECT_SCHEMA_VERSION,
            collection_root,
        };
        let data = toml::to_string_pretty(&marker).context("failed to serialize project marker")?;
        atomic_write(&project_file, data.as_bytes())?;
        Self::load_initialized(&home)
    }

    /// Construct paths without checking initialization. Production command
    /// dispatch should use `resolve_from` or `initialize_from` instead.
    #[cfg(test)]
    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let collection_root = home
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.clone());
        fs::create_dir_all(&collection_root).expect("create test collection root");
        Self::initialize_at(home, &collection_root).expect("initialize test dws project")
    }

    fn validate_layout(&self) -> Result<()> {
        validate_real_directory(&self.home, "dws home")?;
        validate_real_directory(&self.sessions_dir, "sessions directory")?;
        validate_real_directory(&self.workspaces_dir, "workspaces directory")?;
        validate_real_directory(&self.worktrees_dir, "worktrees directory")?;
        Ok(())
    }

    fn validate_initialized(&self) -> Result<()> {
        self.validate_layout()?;
        let marker = read_project_marker(&self.project_file)?;
        let recorded_root = validate_marker(&marker, &self.project_file)?;
        if recorded_root != self.collection_root {
            bail!(
                "project marker collection root changed from {} to {}",
                self.collection_root.display(),
                recorded_root.display()
            );
        }
        Ok(())
    }

    fn repair_initialized(home: &Path) -> Result<Self> {
        let project_file = home.join(PROJECT_FILE_NAME);
        let marker = read_project_marker(&project_file)?;
        validate_marker(&marker, &project_file)?;
        create_layout(home)?;
        Self::load_initialized(home)
    }

    fn load_initialized(home: &Path) -> Result<Self> {
        let project_file = home.join(PROJECT_FILE_NAME);
        let marker = read_project_marker(&project_file)?;
        let collection_root = validate_marker(&marker, &project_file)?;
        let home = canonical_directory(home, "dws home")?;
        let paths = Self::from_parts(home, collection_root);
        paths.validate_initialized().with_context(|| {
            format!(
                "project layout is incomplete; run 'dws init' to repair {}",
                paths.home.display()
            )
        })?;
        Ok(paths)
    }

    fn from_parts(home: PathBuf, collection_root: PathBuf) -> Self {
        Self {
            project_file: home.join(PROJECT_FILE_NAME),
            config_file: home.join("config.toml"),
            sessions_dir: home.join("sessions"),
            workspaces_dir: home.join("workspaces"),
            worktrees_dir: home.join("worktrees"),
            collection_root,
            home,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub editor: EditorConfig,
    #[serde(default)]
    pub file_manager: FileManagerConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileManagerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl Config {
    pub fn load(paths: &DynwsPaths) -> Result<Self> {
        if !paths.config_file.exists() {
            return Ok(Self::default());
        }

        let data = fs::read_to_string(&paths.config_file)
            .with_context(|| format!("failed to read {}", paths.config_file.display()))?;
        let config = toml::from_str(&data)
            .with_context(|| format!("failed to parse {}", paths.config_file.display()))?;
        Ok(config)
    }

    pub fn save(&self, paths: &DynwsPaths) -> Result<()> {
        paths.validate_initialized()?;
        let data = toml::to_string_pretty(self).context("failed to serialize config")?;
        atomic_write(&paths.config_file, data.as_bytes())
    }
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn dynws_home_override(cwd: &Path) -> Result<Option<PathBuf>> {
    let Some(home) = std::env::var_os("DYNWS_HOME") else {
        return Ok(None);
    };
    if home.is_empty() || home.to_string_lossy().trim().is_empty() {
        bail!("DYNWS_HOME is set but empty");
    }
    Ok(Some(absolute_path(PathBuf::from(home), cwd)))
}

fn absolute_path(path: PathBuf, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn path_entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn read_project_marker(project_file: &Path) -> Result<ProjectMarker> {
    let metadata = fs::symlink_metadata(project_file).with_context(|| {
        format!(
            "project marker is missing: {}; run 'dws init' to initialize this project",
            project_file.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "project marker is not a regular file: {}",
            project_file.display()
        );
    }
    let data = fs::read_to_string(project_file).with_context(|| {
        format!(
            "failed to read project marker {}; run 'dws init' to repair the project",
            project_file.display()
        )
    })?;
    toml::from_str(&data).with_context(|| {
        format!(
            "failed to parse project marker {}; fix it or remove it before running 'dws init'",
            project_file.display()
        )
    })
}

fn validate_marker(marker: &ProjectMarker, project_file: &Path) -> Result<PathBuf> {
    if marker.schema_version != PROJECT_SCHEMA_VERSION {
        bail!(
            "unsupported dws project schema {} in {} (supported: {})",
            marker.schema_version,
            project_file.display(),
            PROJECT_SCHEMA_VERSION
        );
    }
    if !marker.collection_root.is_absolute() {
        bail!(
            "collection_root in {} must be an absolute canonical path",
            project_file.display()
        );
    }

    let canonical = canonical_directory(&marker.collection_root, "recorded collection root")?;
    if canonical != marker.collection_root {
        bail!(
            "collection_root in {} is not canonical (expected {})",
            project_file.display(),
            canonical.display()
        );
    }
    Ok(canonical)
}

fn create_layout(home: &Path) -> Result<()> {
    fs::create_dir_all(home).with_context(|| format!("failed to create {}", home.display()))?;
    for directory in ["sessions", "workspaces", "worktrees"] {
        let path = home.join(directory);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        validate_real_directory(&path, directory)?;
    }
    Ok(())
}

fn validate_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("{label} is missing: {}", path.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!("{label} is not a real directory: {}", path.display());
    }
    Ok(())
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {label} {}", path.display()))?;
    validate_real_directory(&canonical, label)?;
    Ok(canonical)
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("cannot atomically write a path without a parent directory")?;
    validate_real_directory(parent, "destination directory")?;
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dws");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let (temp, mut file) = (0_u8..10)
        .find_map(|attempt| {
            let temp = parent.join(format!(
                ".{stem}.{}.{nonce}.{attempt}.tmp",
                std::process::id()
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
            {
                Ok(file) => Some(Ok((temp, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()
        .with_context(|| format!("failed to create a temporary file in {}", parent.display()))?
        .context("failed to allocate a unique temporary config path")?;
    if let Err(error) = file.write_all(data).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("failed to write {}", temp.display()));
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct LegacySession {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    repos: Vec<LegacyRepo>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct LegacyRepo {
    name: String,
    path: PathBuf,
    #[serde(default)]
    kind: LegacyRepoKind,
    #[serde(default)]
    source_repo: Option<PathBuf>,
    #[serde(default)]
    remote_ref: Option<String>,
    #[serde(default)]
    local_branch: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LegacyRepoKind {
    #[default]
    Link,
    Worktree,
}

fn validate_legacy_layout(home: &Path) -> Result<()> {
    let Some(entries) = read_optional_directory(home)? else {
        return Ok(());
    };
    if entries.is_empty() {
        return Ok(());
    }

    let allowed = HashSet::from(["config.toml", "sessions", "workspaces", "worktrees"]);
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !allowed.contains(name.as_str()) {
            bail!(
                "cannot adopt markerless dws directory {}: unexpected entry '{name}'",
                home.display()
            );
        }
    }

    let sessions_dir = home.join("sessions");
    let workspaces_dir = home.join("workspaces");
    let worktrees_dir = home.join("worktrees");
    for (path, label) in [
        (&sessions_dir, "legacy sessions directory"),
        (&workspaces_dir, "legacy workspaces directory"),
        (&worktrees_dir, "legacy worktrees directory"),
    ] {
        validate_real_directory(path, label).with_context(|| {
            format!(
                "cannot adopt incomplete markerless dws directory {}",
                home.display()
            )
        })?;
    }

    let config_file = home.join("config.toml");
    if config_file.try_exists()? {
        let metadata = fs::symlink_metadata(&config_file).with_context(|| {
            format!("failed to inspect legacy config {}", config_file.display())
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!(
                "legacy config is not a regular file: {}",
                config_file.display()
            );
        }
        let data = fs::read_to_string(&config_file)
            .with_context(|| format!("failed to read legacy config {}", config_file.display()))?;
        toml::from_str::<Config>(&data)
            .with_context(|| format!("failed to parse legacy config {}", config_file.display()))?;
    }

    let mut sessions = HashMap::<String, HashMap<String, PathBuf>>::new();
    for entry in fs::read_dir(&sessions_dir)
        .with_context(|| format!("failed to read {}", sessions_dir.display()))?
    {
        let entry = entry.context("failed to read legacy session entry")?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            bail!(
                "invalid entry in legacy sessions directory: {}",
                path.display()
            );
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read legacy session {}", path.display()))?;
        let session: LegacySession = toml::from_str(&data)
            .with_context(|| format!("failed to parse legacy session {}", path.display()))?;
        validate_component(&session.name, "legacy session name")?;
        if normalize_legacy_session_name(&session.name).as_deref() != Some(&session.name) {
            bail!(
                "legacy session name is not normalized in {}: '{}'",
                path.display(),
                session.name
            );
        }
        let file_name = path.file_stem().and_then(|name| name.to_str());
        if file_name != Some(session.name.as_str()) {
            bail!(
                "legacy session filename does not match its name: {}",
                path.display()
            );
        }
        let mut repo_links = HashMap::new();
        let mut repo_paths = HashSet::new();
        for repo in session.repos {
            validate_component(&repo.name, "legacy repo link name")?;
            if !repo.path.is_absolute() {
                bail!(
                    "legacy repo path must be absolute in {}: {}",
                    path.display(),
                    repo.path.display()
                );
            }
            let path_identity = repo
                .path
                .canonicalize()
                .unwrap_or_else(|_| repo.path.clone());
            if repo_links.contains_key(&repo.name) || !repo_paths.insert(path_identity) {
                bail!("duplicate repo link in legacy session {}", path.display());
            }
            match repo.kind {
                LegacyRepoKind::Link => {
                    if repo.source_repo.is_some()
                        || repo.remote_ref.is_some()
                        || repo.local_branch.is_some()
                    {
                        bail!(
                            "normal legacy repo link '{}' contains worktree metadata",
                            repo.name
                        );
                    }
                }
                LegacyRepoKind::Worktree => {
                    let source_repo = repo.source_repo.as_deref().with_context(|| {
                        format!("legacy worktree '{}' is missing source_repo", repo.name)
                    })?;
                    if !source_repo.is_absolute() {
                        bail!(
                            "legacy worktree source must be absolute: {}",
                            source_repo.display()
                        );
                    }
                    if repo.remote_ref.as_deref().is_none_or(str::is_empty)
                        || repo.local_branch.as_deref().is_none_or(str::is_empty)
                    {
                        bail!("legacy worktree '{}' is missing branch metadata", repo.name);
                    }
                    if !canonical_path_is_within(&repo.path, &worktrees_dir) {
                        bail!(
                            "legacy worktree path is outside dws storage: {}",
                            repo.path.display()
                        );
                    }
                }
            }
            repo_links.insert(repo.name, repo.path);
        }
        let _ = (
            &session.description,
            &session.created_at,
            &session.updated_at,
        );
        if sessions.insert(session.name, repo_links).is_some() {
            bail!(
                "duplicate legacy session name in {}",
                sessions_dir.display()
            );
        }
    }

    let mut workspace_names = HashSet::new();
    for entry in fs::read_dir(&workspaces_dir)
        .with_context(|| format!("failed to read {}", workspaces_dir.display()))?
    {
        let entry = entry.context("failed to read legacy workspace entry")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        validate_component(&name, "legacy workspace name")?;
        validate_real_directory(&entry.path(), "legacy workspace")?;
        let expected_links = sessions.get(&name).with_context(|| {
            format!("legacy workspace '{name}' has no matching session metadata")
        })?;
        let mut actual_links = HashSet::new();
        for link in fs::read_dir(entry.path())
            .with_context(|| format!("failed to read legacy workspace '{name}'"))?
        {
            let link = link.context("failed to read legacy workspace link")?;
            let link_name = link.file_name().to_string_lossy().into_owned();
            let metadata = fs::symlink_metadata(link.path())
                .with_context(|| format!("failed to inspect {}", link.path().display()))?;
            if !metadata.file_type().is_symlink() || !actual_links.insert(link_name.clone()) {
                bail!("invalid legacy workspace entry: {}", link.path().display());
            }
            let expected_target = expected_links.get(&link_name).with_context(|| {
                format!(
                    "legacy workspace link '{}' is missing from session metadata",
                    link.path().display()
                )
            })?;
            let target = fs::read_link(link.path())
                .with_context(|| format!("failed to read link {}", link.path().display()))?;
            let target = if target.is_absolute() {
                target
            } else {
                entry.path().join(target)
            };
            let target = target.canonicalize().unwrap_or(target);
            let expected_target = expected_target
                .canonicalize()
                .unwrap_or_else(|_| expected_target.clone());
            if target != expected_target {
                bail!(
                    "legacy workspace link target does not match session metadata: {}",
                    link.path().display()
                );
            }
        }
        if actual_links != expected_links.keys().cloned().collect::<HashSet<_>>() {
            bail!("legacy workspace links do not match session '{name}'");
        }
        workspace_names.insert(name);
    }

    if workspace_names != sessions.keys().cloned().collect::<HashSet<_>>() {
        bail!(
            "legacy sessions and workspaces do not match in {}",
            home.display()
        );
    }
    Ok(())
}

fn read_optional_directory(path: &Path) -> Result<Option<Vec<fs::DirEntry>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!("dws home is not a real directory: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    fs::read_dir(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .map(|entry| entry.context("failed to read dws home entry"))
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn validate_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value != value.trim()
        || value == "."
        || value == ".."
        || Path::new(value).components().count() != 1
        || !matches!(
            Path::new(value).components().next(),
            Some(Component::Normal(_))
        )
    {
        bail!("{label} is not a safe path component: '{value}'");
    }
    Ok(())
}

fn normalize_legacy_session_name(input: &str) -> Option<String> {
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
    (!output.is_empty() && output != "." && output != ".." && !output.contains('/'))
        .then_some(output)
}

fn canonical_path_is_within(path: &Path, root: &Path) -> bool {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialized_paths(temp: &tempfile::TempDir) -> DynwsPaths {
        DynwsPaths::initialize_at(temp.path().join(".dynws"), temp.path()).unwrap()
    }

    #[test]
    fn config_round_trips_defaults_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let paths = initialized_paths(&temp);
        let config = Config {
            editor: EditorConfig {
                default: Some("code".to_string()),
            },
            file_manager: FileManagerConfig {
                default: Some("open".to_string()),
            },
        };

        config.save(&paths).unwrap();

        assert_eq!(Config::load(&paths).unwrap(), config);
        assert!(!fs::read_dir(&paths.home).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn config_save_does_not_initialize_a_project() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("missing");
        let paths = DynwsPaths::from_parts(home, temp.path().to_path_buf());

        assert!(Config::default().save(&paths).is_err());
        assert!(!paths.home.exists());
    }

    #[test]
    fn resolves_nearest_initialized_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let paths = initialized_paths(&temp);
        let nested = temp.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        let resolved = DynwsPaths::resolve_from(&nested).unwrap();

        assert_eq!(resolved.home, paths.home);
        assert_eq!(
            resolved.collection_root,
            temp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn initialization_is_idempotent_and_repairs_managed_directories() {
        let temp = tempfile::tempdir().unwrap();
        let paths = initialized_paths(&temp);
        fs::remove_dir(&paths.worktrees_dir).unwrap();

        let repaired = DynwsPaths::initialize_at(&paths.home, temp.path()).unwrap();

        assert!(repaired.worktrees_dir.is_dir());
        assert_eq!(
            fs::read_to_string(&paths.project_file).unwrap(),
            fs::read_to_string(&repaired.project_file).unwrap()
        );
    }

    #[test]
    fn adopts_a_valid_legacy_layout() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".dynws");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("workspaces")).unwrap();
        fs::create_dir_all(home.join("worktrees")).unwrap();

        let paths = DynwsPaths::initialize_at(&home, temp.path()).unwrap();

        assert!(paths.project_file.is_file());
    }

    #[test]
    fn rejects_ambiguous_legacy_layout_without_mutating_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".dynws");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("unexpected"), "keep").unwrap();

        let error = DynwsPaths::initialize_at(&home, temp.path()).unwrap_err();

        assert!(error.to_string().contains("unexpected entry"));
        assert!(!home.join(PROJECT_FILE_NAME).exists());
        assert_eq!(fs::read_to_string(home.join("unexpected")).unwrap(), "keep");
    }

    #[test]
    fn marker_records_canonical_collection_root_and_schema() {
        let temp = tempfile::tempdir().unwrap();
        let paths = initialized_paths(&temp);
        let marker: ProjectMarker =
            toml::from_str(&fs::read_to_string(paths.project_file).unwrap()).unwrap();

        assert_eq!(marker.schema_version, 1);
        assert_eq!(marker.collection_root, temp.path().canonicalize().unwrap());
    }
}
