use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result, anyhow};

use crate::git::{GitRepoStatus, repo_status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCandidate {
    pub name: String,
    pub path: PathBuf,
    pub git: Option<GitRepoStatus>,
}

#[cfg(test)]
pub fn discover_repos(cwd: &Path) -> Result<Vec<RepoCandidate>> {
    discover_repos_excluding(cwd, None)
}

pub fn discover_repos_excluding(
    cwd: &Path,
    excluded_home: Option<&Path>,
) -> Result<Vec<RepoCandidate>> {
    let excluded_home = excluded_home
        .map(|path| {
            path.canonicalize()
                .with_context(|| format!("failed to canonicalize excluded home {}", path.display()))
        })
        .transpose()?;
    let mut candidates = Vec::new();
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
        if excluded_home
            .as_ref()
            .is_some_and(|excluded| canonical.starts_with(excluded))
        {
            continue;
        }
        candidates.push((name, canonical));
    }

    let worker_count = candidates.len().min(8);
    if worker_count == 0 {
        return Ok(Vec::new());
    }
    let chunk_size = candidates.len().div_ceil(worker_count);
    let mut repos = thread::scope(|scope| -> Result<Vec<RepoCandidate>> {
        let handles = candidates
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|(name, path)| RepoCandidate {
                            name: name.clone(),
                            path: path.clone(),
                            git: repo_status(path).unwrap_or(None),
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();

        let mut repos = Vec::with_capacity(candidates.len());
        for handle in handles {
            repos.extend(
                handle
                    .join()
                    .map_err(|_| anyhow!("repository discovery worker panicked"))?,
            );
        }
        Ok(repos)
    })?;

    sort_repos(&mut repos);
    Ok(repos)
}

fn sort_repos(repos: &mut [RepoCandidate]) {
    repos.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.path.cmp(&right.path))
    });
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
    fn excludes_dynws_storage_and_symlinks_into_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".dynws");
        fs::create_dir_all(home.join("worktrees/repo")).unwrap();
        fs::create_dir(temp.path().join("actual-repo")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            home.join("worktrees/repo"),
            temp.path().join("storage-alias"),
        )
        .unwrap();

        let repos = discover_repos_excluding(temp.path(), Some(&home)).unwrap();

        assert_eq!(
            repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["actual-repo"]
        );
    }

    #[test]
    fn sorting_has_deterministic_case_tiebreakers() {
        let mut repos = vec![
            RepoCandidate {
                name: "repo".to_string(),
                path: PathBuf::from("/collection/repo"),
                git: None,
            },
            RepoCandidate {
                name: "Repo".to_string(),
                path: PathBuf::from("/collection/Repo"),
                git: None,
            },
        ];

        sort_repos(&mut repos);

        assert_eq!(
            repos
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Repo", "repo"]
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
