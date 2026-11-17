use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::session::{RepoKind, normalize_name};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum InputFormat {
    Json,
    Toml,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub name: String,
    pub repos: Vec<ManifestRepo>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRepo {
    pub path: PathBuf,
    pub name: Option<String>,
    pub kind: RepoKind,
    pub branch: Option<String>,
    pub new_branch: Option<String>,
    pub base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreationRecord {
    pub schema_version: u32,
    pub repos: Vec<CreationRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreationRepo {
    pub name: String,
    pub input: PathBuf,
    pub kind: RepoKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_commit: Option<String>,
}

pub fn load(file: &Path, format: Option<InputFormat>, cwd: &Path) -> Result<(Manifest, PathBuf)> {
    let stdin = file == Path::new("-");
    let format = format
        .or_else(|| match file.extension().and_then(|s| s.to_str()) {
            Some("json") => Some(InputFormat::Json),
            Some("toml") => Some(InputFormat::Toml),
            _ => None,
        })
        .context("specify --input-format json|toml for stdin or an unrecognized file extension")?;
    let mut data = String::new();
    let base = if stdin {
        std::io::stdin()
            .read_to_string(&mut data)
            .context("failed to read manifest from stdin")?;
        cwd.to_path_buf()
    } else {
        let file = cwd.join(file);
        data = fs::read_to_string(&file)
            .with_context(|| format!("failed to read {}", file.display()))?;
        file.parent()
            .context("manifest has no parent")?
            .to_path_buf()
    };
    let manifest: Manifest = match format {
        InputFormat::Json => {
            serde_json::from_str(&data).context("invalid JSON workspace manifest")?
        }
        InputFormat::Toml => toml::from_str(&data).context("invalid TOML workspace manifest")?,
    };
    Ok((manifest, base))
}

pub fn normalize(manifest: Manifest, base: &Path) -> Result<(String, CreationRecord)> {
    if manifest.schema_version != 1 {
        bail!(
            "unsupported manifest schema {}; supported: 1",
            manifest.schema_version
        );
    }
    let suggested = normalize_name(&manifest.name)?;
    if suggested != manifest.name {
        bail!(
            "invalid workspace name '{}'; use '{suggested}'",
            manifest.name
        );
    }
    if manifest.repos.is_empty() {
        bail!("at least one repository is required");
    }
    let mut names = HashSet::new();
    let mut inputs = HashSet::new();
    let mut repos = Vec::new();
    for repo in manifest.repos {
        let input = base
            .join(&repo.path)
            .canonicalize()
            .with_context(|| format!("cannot resolve repository {}", repo.path.display()))?;
        if !input.is_dir() {
            bail!("repository path is not a directory: {}", input.display());
        }
        let name = repo
            .name
            .or_else(|| input.file_name()?.to_str().map(str::to_owned))
            .context("repository needs an explicit name")?;
        crate::session::validate_link_name(&name)?;
        if !names.insert(name.clone()) {
            bail!("duplicate repository name '{name}'; provide explicit per-repository names");
        }
        if !inputs.insert(input.clone()) {
            bail!("duplicate canonical repository path: {}", input.display());
        }
        match repo.kind {
            RepoKind::Link
                if repo.branch.is_some() || repo.new_branch.is_some() || repo.base.is_some() =>
            {
                bail!("link '{name}' cannot contain branch fields")
            }
            RepoKind::Worktree
                if !matches!(
                    (&repo.branch, &repo.new_branch, &repo.base),
                    (Some(_), None, None) | (None, Some(_), Some(_))
                ) =>
            {
                bail!("worktree '{name}' requires branch OR new_branch and base")
            }
            _ => {}
        }
        repos.push(CreationRepo {
            name,
            input,
            kind: repo.kind,
            source: None,
            branch: repo.branch,
            new_branch: repo.new_branch,
            base: repo.base,
            initial_commit: None,
        });
    }
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((
        manifest.name,
        CreationRecord {
            schema_version: 1,
            repos,
        },
    ))
}
