use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::git::{GitRepoStatus, repo_status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCandidate {
    pub name: String,
    pub path: PathBuf,
    pub git: Option<GitRepoStatus>,
}

pub fn discover_repos(cwd: &Path) -> Result<Vec<RepoCandidate>> {
    let mut repos = Vec::new();
    for entry in fs::read_dir(cwd).with_context(|| format!("failed to read {}", cwd.display()))? {
        let entry = entry.context("failed to read directory entry")?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))?;
        let git = repo_status(&canonical).unwrap_or(None);
        repos.push(RepoCandidate {
            name,
            path: canonical,
            git,
        });
    }

    repos.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(repos)
}

pub fn fuzzy_matches(query: &str, candidate: &RepoCandidate) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }

    let haystack = format!("{} {}", candidate.name, candidate.path.display()).to_lowercase();
    is_subsequence(&query.to_lowercase(), &haystack)
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack = haystack.chars();
    needle
        .chars()
        .all(|needle_char| haystack.any(|hay_char| hay_char == needle_char))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_immediate_subdirectories() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("beta")).unwrap();
        fs::create_dir(temp.path().join("alpha")).unwrap();
        fs::write(temp.path().join("file.txt"), "ignored").unwrap();

        let repos = discover_repos(temp.path()).unwrap();

        assert_eq!(
            repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn fuzzy_match_uses_subsequence_matching() {
        let candidate = RepoCandidate {
            name: "frontend-service".to_string(),
            path: PathBuf::from("/work/frontend-service"),
            git: None,
        };

        assert!(fuzzy_matches("fs", &candidate));
        assert!(fuzzy_matches("front", &candidate));
        assert!(!fuzzy_matches("zz", &candidate));
    }
}
