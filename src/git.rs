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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginBranch {
    pub name: String,
    pub remote_ref: String,
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

pub fn list_origin_branches(repo_path: &Path) -> Result<Vec<OriginBranch>> {
    if repo_status(repo_path)?.is_none() {
        bail!("{} is not a git repository", repo_path.display());
    }

    fetch_origin(repo_path)?;

    let Some(output) = git_output(repo_path, ["branch", "-r", "--format=%(refname:short)"])? else {
        bail!("git is not installed or not on PATH");
    };
    if !output.status.success() {
        bail!("git branch -r failed: {}", output_string(&output.stderr));
    }

    Ok(parse_origin_branches(&output_string(&output.stdout)))
}

pub fn fetch_origin(repo_path: &Path) -> Result<()> {
    let Some(output) = git_output(repo_path, ["fetch", "origin", "--prune"])? else {
        bail!("git is not installed or not on PATH");
    };
    if output.status.success() {
        return Ok(());
    }

    let prune_error = combined_git_error(&output);
    if is_ref_lock_fetch_error(&prune_error) {
        let Some(fallback) = git_output(repo_path, ["fetch", "origin"])? else {
            bail!("git is not installed or not on PATH");
        };
        if fallback.status.success() {
            return Ok(());
        }
        bail!(
            "git fetch origin failed after prune fallback: {}",
            combined_git_error(&fallback)
        );
    }

    bail!("git fetch origin failed: {prune_error}");
}

fn is_ref_lock_fetch_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("could not delete references")
        || message.contains("cannot lock ref")
        || message.contains(".lock")
        || message.contains("another git process")
        || message.contains("unable to create")
        || message.contains("is at") && message.contains("but expected")
}

pub fn parse_origin_branches(output: &str) -> Vec<OriginBranch> {
    let mut branches = output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("origin/"))
        .filter(|line| *line != "origin/HEAD")
        .filter(|line| !line.starts_with("origin/HEAD ->"))
        .filter_map(|remote_ref| {
            let name = remote_ref.strip_prefix("origin/")?.trim();
            (!name.is_empty()).then(|| OriginBranch {
                name: name.to_string(),
                remote_ref: remote_ref.to_string(),
            })
        })
        .collect::<Vec<_>>();
    branches.sort_by(|left, right| left.name.cmp(&right.name));
    branches.dedup_by(|left, right| left.name == right.name);
    branches
}

pub fn branch_worktree_slug(branch: &str) -> Result<String> {
    normalize_name(branch)
}

pub fn worktree_target_path(
    repo_path: &Path,
    worktrees_root: &Path,
    branch: &OriginBranch,
) -> Result<PathBuf> {
    let repo_name = repo_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("repo path has no valid terminal component")?;
    Ok(worktrees_root
        .join(repo_name)
        .join(branch_worktree_slug(&branch.name)?))
}

pub fn create_worktree_from_origin(
    repo_path: &Path,
    worktrees_root: &Path,
    branch: &OriginBranch,
) -> Result<PathBuf> {
    if repo_status(repo_path)?.is_none() {
        bail!("{} is not a git repository", repo_path.display());
    }

    let repo_name = repo_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("repo path has no valid terminal component")?;
    let branch_slug = branch_worktree_slug(&branch.name)?;
    let target = worktrees_root.join(repo_name).join(&branch_slug);
    if target.exists() {
        if target.is_dir() && repo_status(&target)?.is_some() {
            return Ok(target);
        }
        bail!(
            "worktree target already exists but is not a git worktree: {}",
            target.display()
        );
    }

    fs::create_dir_all(
        target
            .parent()
            .context("worktree target has no parent directory")?,
    )
    .with_context(|| format!("failed to create {}", worktrees_root.display()))?;

    let first = create_or_add_worktree_branch(repo_path, &target, &branch.name, &branch.remote_ref);
    match first {
        Ok(()) => Ok(target),
        Err(error) if is_already_checked_out_error(&error) => {
            let fallback = format!("dws/{repo_name}/{branch_slug}");
            create_or_add_worktree_branch(repo_path, &target, &fallback, &branch.remote_ref)?;
            Ok(target)
        }
        Err(error) => Err(error),
    }
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

pub fn is_linked_worktree(path: &Path) -> Result<bool> {
    if repo_status(path)?.is_none() {
        return Ok(false);
    }

    Ok(path.join(".git").is_file())
}

pub fn remove_worktree(path: &Path) -> Result<()> {
    if !is_linked_worktree(path)? {
        bail!("{} is not a linked git worktree", path.display());
    }

    let output = git_command_output(path, &["worktree", "remove", path_arg(path)])?;
    if !output.status.success() {
        bail!(combined_git_error(&output));
    }

    Ok(())
}

fn create_or_add_worktree_branch(
    repo_path: &Path,
    target: &Path,
    local_branch: &str,
    remote_ref: &str,
) -> Result<()> {
    if local_branch_exists(repo_path, local_branch)? {
        let output = git_command_output(
            repo_path,
            &["worktree", "add", path_arg(target), local_branch],
        )?;
        if !output.status.success() {
            bail!(combined_git_error(&output));
        }
        return Ok(());
    }

    let output = git_command_output(
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            local_branch,
            path_arg(target),
            remote_ref,
        ],
    )?;
    if !output.status.success() {
        bail!(combined_git_error(&output));
    }

    let output = git_command_output(
        target,
        &["branch", "--set-upstream-to", remote_ref, local_branch],
    )?;
    if !output.status.success() {
        bail!(combined_git_error(&output));
    }

    Ok(())
}

fn local_branch_exists(repo_path: &Path, branch: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch}");
    let output = git_command_output(repo_path, &["show-ref", "--verify", "--quiet", &ref_name])?;
    Ok(output.status.success())
}

fn is_already_checked_out_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("already checked out")
        || message.contains("is already used by worktree")
        || message.contains("is checked out")
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

fn git_command_output(path: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .context("failed to run git")
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

fn combined_git_error(output: &Output) -> String {
    let stderr = output_string(&output.stderr);
    if stderr.is_empty() {
        output_string(&output.stdout)
    } else {
        stderr
    }
}

fn path_arg(path: &Path) -> &str {
    path.to_str().unwrap_or("")
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

    fn commit_file(repo: &Path, file: &str, contents: &str, message: &str) {
        fs::write(repo.join(file), contents).unwrap();
        run_git(repo, &["add", file]);
        run_git(
            repo,
            &[
                "-c",
                "user.name=dynws",
                "-c",
                "user.email=dynws@example.invalid",
                "commit",
                "-m",
                message,
            ],
        );
    }

    fn repo_with_origin(temp: &Path) -> (PathBuf, PathBuf) {
        let origin = temp.join("origin.git");
        let repo = temp.join("repo");
        fs::create_dir_all(&origin).unwrap();
        run_git(&origin, &["init", "--bare"]);

        let status = Command::new("git")
            .arg("clone")
            .arg(&origin)
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git clone failed");

        run_git(&repo, &["checkout", "-b", "main"]);
        commit_file(&repo, "README.md", "main", "main");
        run_git(&repo, &["push", "-u", "origin", "main"]);

        run_git(&repo, &["checkout", "-b", "feature/demo"]);
        commit_file(&repo, "feature.txt", "feature", "feature");
        run_git(&repo, &["push", "-u", "origin", "feature/demo"]);
        run_git(&repo, &["checkout", "main"]);
        run_git(&repo, &["fetch", "origin"]);

        (repo, origin)
    }

    #[test]
    fn parses_origin_branches() {
        let branches = parse_origin_branches(
            r#"
origin/HEAD
origin/HEAD -> origin/main
origin/main
origin/feature/demo
upstream/ignored
origin/main
"#,
        );

        assert_eq!(
            branches,
            vec![
                OriginBranch {
                    name: "feature/demo".to_string(),
                    remote_ref: "origin/feature/demo".to_string(),
                },
                OriginBranch {
                    name: "main".to_string(),
                    remote_ref: "origin/main".to_string(),
                },
            ]
        );
    }

    #[test]
    fn builds_stable_worktree_slug() {
        assert_eq!(
            branch_worktree_slug("feature/API Demo").unwrap(),
            "feature-api-demo"
        );
    }

    #[test]
    fn detects_ref_lock_fetch_errors() {
        assert!(is_ref_lock_fetch_error(
            "could not delete references: cannot lock ref 'refs/remotes/origin/main'"
        ));
        assert!(is_ref_lock_fetch_error(
            "Unable to create '/repo/.git/refs/remotes/origin/main.lock': File exists. Another git process seems to be running"
        ));
        assert!(!is_ref_lock_fetch_error(
            "fatal: 'origin' does not appear to be a git repository"
        ));
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

    #[test]
    fn lists_origin_branches() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());

        let branches = list_origin_branches(&repo).unwrap();

        assert!(branches.iter().any(|branch| branch.name == "main"));
        assert!(branches.iter().any(|branch| branch.name == "feature/demo"));
        assert!(
            branches
                .iter()
                .all(|branch| branch.remote_ref.starts_with("origin/"))
        );
    }

    #[test]
    fn list_origin_branches_fetches_latest_origin_refs() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let (repo, origin) = repo_with_origin(temp.path());
        let contributor = temp.path().join("contributor");
        let status = Command::new("git")
            .arg("clone")
            .arg(&origin)
            .arg(&contributor)
            .status()
            .unwrap();
        assert!(status.success(), "git clone failed");
        run_git(&contributor, &["checkout", "-b", "fresh/remote"]);
        commit_file(&contributor, "fresh.txt", "fresh", "fresh");
        run_git(&contributor, &["push", "-u", "origin", "fresh/remote"]);

        let branches = list_origin_branches(&repo).unwrap();

        assert!(branches.iter().any(|branch| branch.name == "fresh/remote"));
    }

    #[test]
    fn creates_worktree_from_origin_branch() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "feature/demo".to_string(),
            remote_ref: "origin/feature/demo".to_string(),
        };

        let target =
            create_worktree_from_origin(&repo, &temp.path().join("worktrees"), &branch).unwrap();

        assert!(target.exists());
        assert!(target.ends_with("repo/feature-demo"));
        assert_eq!(
            git_output_text(&target, &["branch", "--show-current"]),
            "feature/demo"
        );
    }

    #[test]
    fn falls_back_when_local_branch_is_checked_out() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "main".to_string(),
            remote_ref: "origin/main".to_string(),
        };

        let target =
            create_worktree_from_origin(&repo, &temp.path().join("worktrees"), &branch).unwrap();

        assert!(target.exists());
        assert_eq!(
            git_output_text(&target, &["branch", "--show-current"]),
            "dws/repo/main"
        );
    }
}
