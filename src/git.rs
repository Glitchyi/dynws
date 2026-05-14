use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use crate::session::normalize_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepoStatus {
    pub branch: String,
    pub dirty: bool,
}

pub fn repo_status(path: &Path) -> Result<Option<GitRepoStatus>> {
    let Some(output) = git_output(path, ["rev-parse", "--is-inside-work-tree"])? else {
        return Ok(None);
    };
    if !output.status.success() || output_string(&output.stdout) != "true" {
        return Ok(None);
    }

    let branch = match git_output(path, ["branch", "--show-current"])? {
        Some(output) if output.status.success() => {
            let branch = output_string(&output.stdout);
            if branch.is_empty() {
                detached_head(path)?
            } else {
                branch
            }
        }
        _ => detached_head(path)?,
    };

    let dirty = match git_output(path, ["status", "--porcelain"])? {
        Some(output) if output.status.success() => !output_string(&output.stdout).is_empty(),
        _ => false,
    };

    Ok(Some(GitRepoStatus { branch, dirty }))
}

pub fn create_worktree(repo_path: &Path, worktrees_root: &Path, name: &str) -> Result<PathBuf> {
    if repo_status(repo_path)?.is_none() {
        bail!("{} is not a git repository", repo_path.display());
    }

    let worktree_name = normalize_name(name)?;
    let repo_name = repo_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("repo path has no valid terminal component")?;
    let target = worktrees_root.join(repo_name).join(&worktree_name);
    if target.exists() {
        bail!("worktree already exists: {}", target.display());
    }
    fs::create_dir_all(
        target
            .parent()
            .context("worktree target has no parent directory")?,
    )
    .with_context(|| format!("failed to create {}", worktrees_root.display()))?;

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&worktree_name)
        .arg(&target)
        .arg("HEAD")
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        bail!("git worktree add failed: {}", output_string(&output.stderr));
    }

    Ok(target)
}

fn detached_head(path: &Path) -> Result<String> {
    match git_output(path, ["rev-parse", "--short", "HEAD"])? {
        Some(output) if output.status.success() => {
            let head = output_string(&output.stdout);
            if head.is_empty() {
                Ok("detached".to_string())
            } else {
                Ok(format!("detached@{head}"))
            }
        }
        _ => Ok("detached".to_string()),
    }
}

fn git_output<const N: usize>(path: &Path, args: [&str; N]) -> Result<Option<Output>> {
    match Command::new("git").arg("-C").arg(path).args(args).output() {
        Ok(output) => Ok(Some(output)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to run git"),
    }
}

fn output_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn non_git_directory_has_no_status() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(repo_status(temp.path()).unwrap(), None);
    }

    #[test]
    fn reports_git_branch_and_dirty_status() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init", "-b", "main"]);
        fs::write(temp.path().join("README.md"), "hello").unwrap();

        let status = repo_status(temp.path()).unwrap().unwrap();

        assert_eq!(status.branch, "main");
        assert!(status.dirty);
    }

    #[test]
    fn creates_worktree_under_dynws_home() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
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

        let target =
            create_worktree(&repo, &temp.path().join("home/worktrees"), "Feature A").unwrap();

        assert!(target.exists());
        assert!(target.starts_with(temp.path().join("home/worktrees/repo")));
    }
}
