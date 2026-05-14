use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct DynwsPaths {
    pub home: PathBuf,
    pub config_file: PathBuf,
    pub sessions_dir: PathBuf,
    pub workspaces_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

impl DynwsPaths {
    pub fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir().context("failed to read current directory")?;
        Self::resolve_from(&cwd)
    }

    pub fn resolve_from(cwd: &Path) -> Result<Self> {
        if let Ok(home) = std::env::var("DYNWS_HOME") {
            let trimmed = home.trim();
            if trimmed.is_empty() {
                bail!("DYNWS_HOME is set but empty");
            }
            return Ok(Self::from_home(PathBuf::from(trimmed)));
        }

        Ok(Self::from_home(cwd.join(".dynws")))
    }

    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            config_file: home.join("config.toml"),
            sessions_dir: home.join("sessions"),
            workspaces_dir: home.join("workspaces"),
            worktrees_dir: home.join("worktrees"),
            home,
        }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.home)
            .with_context(|| format!("failed to create {}", self.home.display()))?;
        fs::create_dir_all(&self.sessions_dir)
            .with_context(|| format!("failed to create {}", self.sessions_dir.display()))?;
        fs::create_dir_all(&self.workspaces_dir)
            .with_context(|| format!("failed to create {}", self.workspaces_dir.display()))?;
        fs::create_dir_all(&self.worktrees_dir)
            .with_context(|| format!("failed to create {}", self.worktrees_dir.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub editor: EditorConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorConfig {
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
        paths.ensure_layout()?;
        let data = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&paths.config_file, data)
            .with_context(|| format!("failed to write {}", paths.config_file.display()))?;
        Ok(())
    }
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_default_editor() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DynwsPaths::from_home(temp.path());
        let config = Config {
            editor: EditorConfig {
                default: Some("code".to_string()),
            },
        };

        config.save(&paths).unwrap();
        let loaded = Config::load(&paths).unwrap();

        assert_eq!(loaded, config);
        assert!(paths.config_file.exists());
    }

    #[test]
    fn from_home_builds_expected_layout() {
        let paths = DynwsPaths::from_home("/tmp/dynws-test");

        assert_eq!(
            paths.config_file,
            PathBuf::from("/tmp/dynws-test/config.toml")
        );
        assert_eq!(
            paths.sessions_dir,
            PathBuf::from("/tmp/dynws-test/sessions")
        );
        assert_eq!(
            paths.workspaces_dir,
            PathBuf::from("/tmp/dynws-test/workspaces")
        );
        assert_eq!(
            paths.worktrees_dir,
            PathBuf::from("/tmp/dynws-test/worktrees")
        );
    }
}
