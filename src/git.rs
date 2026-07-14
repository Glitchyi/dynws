use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

#[cfg(test)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub source_repo: PathBuf,
    pub local_branch: Option<String>,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOutcome {
    pub path: PathBuf,
    pub source_repo: PathBuf,
    pub remote_ref: String,
    pub local_branch: String,
    pub created: bool,
}

pub fn repo_status(path: &Path) -> Result<Option<GitRepoStatus>> {
    repo_status_with_program(path, OsStr::new("git"))
}

fn repo_status_with_program(path: &Path, program: &OsStr) -> Result<Option<GitRepoStatus>> {
    let resolved_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut command = Command::new(program);
    command
        .arg("-C")
        .arg(&resolved_path)
        .args(["status", "--porcelain=v2", "--branch"]);
    if let Some(parent) = resolved_path.parent().filter(|parent| parent.is_absolute()) {
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to run git"),
    };
    if !output.status.success() {
        return Ok(None);
    }

    Ok(parse_repo_status(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_repo_status(output: &str) -> Option<GitRepoStatus> {
    let mut head = None;
    let mut oid = None;
    let mut dirty = false;

    for line in output.lines() {
        if let Some(value) = line.strip_prefix("# branch.head ") {
            head = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("# branch.oid ") {
            oid = Some(value.trim());
        } else if !line.is_empty() && !line.starts_with("# ") {
            dirty = true;
        }
    }

    let branch = match head? {
        "(detached)" => oid
            .filter(|value| !value.is_empty() && *value != "(initial)")
            .map(|value| format!("detached@{}", &value[..value.len().min(7)]))
            .unwrap_or_else(|| "detached".to_string()),
        branch => branch.to_string(),
    };
    Some(GitRepoStatus { branch, dirty })
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
    if branch.is_empty() {
        bail!("branch name cannot be empty");
    }

    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    const CHUNK_LEN: usize = 120;
    let mut encoded = String::new();
    let mut buffer = 0_u16;
    let mut bits = 0_u8;

    for byte in branch.bytes() {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            encoded.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
        buffer &= (1_u16 << bits).saturating_sub(1);
    }
    if bits > 0 {
        encoded.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }

    let mut path = String::from("b-");
    for (index, chunk) in encoded.as_bytes().chunks(CHUNK_LEN).enumerate() {
        if index > 0 {
            path.push('/');
        }
        // The alphabet above is ASCII-only.
        path.push_str(std::str::from_utf8(chunk).expect("base32 output is valid UTF-8"));
    }
    Ok(path)
}

#[cfg(test)]
fn decode_branch_worktree_slug(encoded: &str) -> Result<String> {
    let payload = encoded
        .strip_prefix("b-")
        .context("worktree branch encoding is missing the 'b-' prefix")?;
    if payload.is_empty() {
        bail!("worktree branch encoding is empty");
    }

    let mut bytes = Vec::new();
    let mut buffer = 0_u16;
    let mut bits = 0_u8;
    for character in payload.bytes().filter(|byte| *byte != b'/') {
        let value = match character {
            b'a'..=b'z' => character - b'a',
            b'2'..=b'7' => character - b'2' + 26,
            _ => bail!("invalid character in worktree branch encoding"),
        };
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1_u16 << bits).saturating_sub(1);
        }
    }
    if bits > 0 && buffer != 0 {
        bail!("invalid trailing bits in worktree branch encoding");
    }

    let branch = String::from_utf8(bytes).context("worktree branch encoding is not UTF-8")?;
    if branch_worktree_slug(&branch)? != encoded {
        bail!("worktree branch encoding is not canonical");
    }
    Ok(branch)
}

#[cfg(test)]
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

#[cfg(test)]
pub fn create_worktree_from_origin(
    repo_path: &Path,
    worktrees_root: &Path,
    branch: &OriginBranch,
) -> Result<PathBuf> {
    Ok(ensure_worktree_from_origin(repo_path, worktrees_root, branch)?.path)
}

#[cfg(test)]
pub fn create_worktree(repo_path: &Path, worktrees_root: &Path, name: &str) -> Result<PathBuf> {
    if repo_status(repo_path)?.is_none() {
        bail!("{} is not a git repository", repo_path.display());
    }

    let local_branch = normalize_name(name)?;
    let repo_name = repo_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("repo path has no valid terminal component")?;
    let target = worktrees_root.join(repo_name).join(&local_branch);
    if target.try_exists()? {
        bail!("worktree already exists: {}", target.display());
    }
    fs::create_dir_all(
        target
            .parent()
            .context("worktree target has no parent directory")?,
    )?;
    let output = git_worktree_add(repo_path, &target, Some(&local_branch), "HEAD")?;
    if !output.status.success() {
        bail!("git worktree add failed: {}", combined_git_error(&output));
    }
    Ok(target)
}

pub fn ensure_worktree_from_origin(
    repo_path: &Path,
    worktrees_root: &Path,
    branch: &OriginBranch,
) -> Result<WorktreeOutcome> {
    if repo_status(repo_path)?.is_none() {
        bail!("{} is not a git repository", repo_path.display());
    }
    if branch.remote_ref != format!("origin/{}", branch.name) {
        bail!(
            "remote ref '{}' does not match origin branch '{}'",
            branch.remote_ref,
            branch.name
        );
    }

    let repo_name = repo_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("repo path has no valid terminal component")?;
    let branch_slug = branch_worktree_slug(&branch.name)?;
    let target = worktrees_root.join(repo_name).join(&branch_slug);
    let source_repo = source_repository(repo_path)?;
    let fallback_branch = fallback_branch_name(repo_name, &branch_slug)?;
    if target
        .try_exists()
        .with_context(|| format!("failed to inspect {}", target.display()))?
    {
        if target
            .symlink_metadata()
            .with_context(|| format!("failed to inspect {}", target.display()))?
            .file_type()
            .is_symlink()
        {
            bail!(
                "refusing to reuse symlinked worktree target {}",
                target.display()
            );
        }
        let info = validate_reusable_worktree(
            &target,
            worktrees_root,
            &source_repo,
            branch,
            &fallback_branch,
        )?;
        let local_branch = info
            .local_branch
            .context("validated worktree unexpectedly has a detached HEAD")?;
        return Ok(WorktreeOutcome {
            path: info.path,
            source_repo,
            remote_ref: branch.remote_ref.clone(),
            local_branch,
            created: false,
        });
    }

    fs::create_dir_all(
        target
            .parent()
            .context("worktree target has no parent directory")?,
    )
    .with_context(|| format!("failed to create {}", worktrees_root.display()))?;

    let first = create_or_add_worktree_branch(repo_path, &target, &branch.name, &branch.remote_ref);
    let created_local_branch = match first {
        Ok(()) => branch.name.clone(),
        Err(error) if needs_fallback_branch(&error) && !target.try_exists().unwrap_or(true) => {
            match create_or_add_worktree_branch(
                repo_path,
                &target,
                &fallback_branch,
                &branch.remote_ref,
            ) {
                Ok(()) => fallback_branch.clone(),
                Err(fallback_error) => {
                    return Err(with_creation_cleanup(repo_path, &target, fallback_error));
                }
            }
        }
        Err(error) => return Err(with_creation_cleanup(repo_path, &target, error)),
    };

    let info = match validate_reusable_worktree(
        &target,
        worktrees_root,
        &source_repo,
        branch,
        &fallback_branch,
    ) {
        Ok(info) => info,
        Err(error) => return Err(with_creation_cleanup(repo_path, &target, error)),
    };
    let local_branch = info
        .local_branch
        .context("validated worktree unexpectedly has a detached HEAD")?;
    if local_branch != created_local_branch {
        return Err(with_creation_cleanup(
            repo_path,
            &target,
            anyhow::anyhow!(
                "created worktree is on branch '{local_branch}', expected '{created_local_branch}'"
            ),
        ));
    }

    Ok(WorktreeOutcome {
        path: info.path,
        source_repo,
        remote_ref: branch.remote_ref.clone(),
        local_branch,
        created: true,
    })
}

pub fn is_linked_worktree(path: &Path) -> Result<bool> {
    Ok(inspect_worktree_inner(path, false)?.is_some())
}

pub fn remove_worktree(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .file_type()
        .is_symlink()
    {
        bail!(
            "refusing to remove symlinked worktree path {}",
            path.display()
        );
    }
    let Some(info) = inspect_worktree_inner(path, false)? else {
        bail!("{} is not a linked git worktree", path.display());
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(&info.source_repo)
        .args(["worktree", "remove"])
        .arg(&info.path)
        .output()
        .context("failed to run git worktree remove")?;
    if !output.status.success() {
        bail!(
            "failed to safely remove worktree {}: {}",
            info.path.display(),
            combined_git_error(&output)
        );
    }

    Ok(())
}

pub fn inspect_worktree(path: &Path) -> Result<Option<WorktreeInfo>> {
    inspect_worktree_inner(path, true)
}

fn inspect_worktree_inner(path: &Path, include_upstream: bool) -> Result<Option<WorktreeInfo>> {
    if !path.join(".git").is_file() {
        return Ok(None);
    }

    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;
    let records = worktree_records(path)?;
    let Some(record) = records
        .iter()
        .find(|record| same_path(&record.path, &canonical))
    else {
        bail!(
            "{} has a worktree .git file but is not registered with its source repository",
            path.display()
        );
    };
    let source_repo = records
        .first()
        .context("git returned an empty worktree list")?
        .path
        .canonicalize()
        .with_context(|| "failed to canonicalize the source repository")?;
    let upstream = match (&record.local_branch, include_upstream) {
        (Some(local_branch), true) => branch_upstream(path, local_branch)?,
        _ => None,
    };

    Ok(Some(WorktreeInfo {
        path: canonical,
        source_repo,
        local_branch: record.local_branch.clone(),
        upstream,
    }))
}

fn validate_reusable_worktree(
    target: &Path,
    worktrees_root: &Path,
    source_repo: &Path,
    branch: &OriginBranch,
    fallback_branch: &str,
) -> Result<WorktreeInfo> {
    let info = inspect_worktree(target)?.with_context(|| {
        format!(
            "worktree target already exists but is not a linked git worktree: {}",
            target.display()
        )
    })?;
    let canonical_root = worktrees_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", worktrees_root.display()))?;
    if !info.path.starts_with(&canonical_root) {
        bail!(
            "worktree target {} escapes managed root {}",
            info.path.display(),
            canonical_root.display()
        );
    }
    if !same_path(&info.source_repo, source_repo) {
        bail!(
            "worktree target {} belongs to {}, not {}",
            target.display(),
            info.source_repo.display(),
            source_repo.display()
        );
    }

    let local_branch = info
        .local_branch
        .as_deref()
        .with_context(|| format!("worktree target {} has a detached HEAD", target.display()))?;
    if local_branch != branch.name && local_branch != fallback_branch {
        bail!(
            "worktree target {} is on branch '{}', expected '{}'",
            target.display(),
            local_branch,
            branch.name
        );
    }
    if info.upstream.as_deref() != Some(branch.remote_ref.as_str()) {
        bail!(
            "worktree target {} tracks {:?}, expected '{}'",
            target.display(),
            info.upstream,
            branch.remote_ref
        );
    }
    Ok(info)
}

fn fallback_branch_name(repo_name: &str, branch_slug: &str) -> Result<String> {
    let encoded_repo = branch_worktree_slug(repo_name)?;
    Ok(format!("dws/{encoded_repo}/{branch_slug}"))
}

fn with_creation_cleanup(repo_path: &Path, target: &Path, error: anyhow::Error) -> anyhow::Error {
    match cleanup_new_worktree(repo_path, target) {
        Ok(()) => error,
        Err(cleanup) => anyhow::anyhow!(
            "{error:#}; failed to roll back {}: {cleanup:#}",
            target.display()
        ),
    }
}

fn cleanup_new_worktree(repo_path: &Path, target: &Path) -> Result<()> {
    if !target.try_exists().unwrap_or(false) {
        return Ok(());
    }
    if target
        .symlink_metadata()
        .with_context(|| format!("failed to inspect {}", target.display()))?
        .file_type()
        .is_symlink()
    {
        bail!(
            "refusing to remove symlinked rollback target {}",
            target.display()
        );
    }

    let source_repo = source_repository(repo_path)?;
    let Some(info) = inspect_worktree_inner(target, false)? else {
        return fs::remove_dir(target).with_context(|| {
            format!(
                "refusing to recursively remove unregistered path {}",
                target.display()
            )
        });
    };
    if !same_path(&info.source_repo, &source_repo) {
        bail!(
            "refusing to remove worktree owned by {}",
            info.source_repo.display()
        );
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(&source_repo)
        .args(["worktree", "remove", "--force"])
        .arg(&info.path)
        .output()
        .context("failed to run git worktree remove during rollback")?;
    if !output.status.success() {
        bail!(
            "git refused to remove newly-created worktree {}: {}",
            target.display(),
            combined_git_error(&output)
        );
    }
    Ok(())
}

#[derive(Debug)]
struct WorktreeRecord {
    path: PathBuf,
    local_branch: Option<String>,
}

fn source_repository(repo_path: &Path) -> Result<PathBuf> {
    worktree_records(repo_path)?
        .first()
        .context("git returned an empty worktree list")?
        .path
        .canonicalize()
        .context("failed to canonicalize the source repository")
}

fn worktree_records(repo_path: &Path) -> Result<Vec<WorktreeRecord>> {
    let output = git_command_output(repo_path, ["worktree", "list", "--porcelain"])?;
    if !output.status.success() {
        bail!("git worktree list failed: {}", combined_git_error(&output));
    }
    Ok(parse_worktree_records(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_worktree_records(output: &str) -> Vec<WorktreeRecord> {
    output
        .split("\n\n")
        .filter_map(|block| {
            let mut path = None;
            let mut local_branch = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(value));
                } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
                    local_branch = Some(value.to_string());
                }
            }
            path.map(|path| WorktreeRecord { path, local_branch })
        })
        .collect()
}

fn branch_upstream(repo_path: &Path, local_branch: &str) -> Result<Option<String>> {
    let reference = format!("refs/heads/{local_branch}");
    let output = git_command_output(
        repo_path,
        ["for-each-ref", "--format=%(upstream:short)", &reference],
    )?;
    if !output.status.success() {
        bail!(
            "failed to inspect worktree upstream: {}",
            combined_git_error(&output)
        );
    }
    let upstream = output_string(&output.stdout);
    Ok((!upstream.is_empty()).then_some(upstream))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn create_or_add_worktree_branch(
    repo_path: &Path,
    target: &Path,
    local_branch: &str,
    remote_ref: &str,
) -> Result<()> {
    if local_branch_exists(repo_path, local_branch)? {
        let upstream = branch_upstream(repo_path, local_branch)?;
        if upstream.as_deref() != Some(remote_ref) {
            bail!(
                "local branch '{local_branch}' does not track '{remote_ref}' (tracks {upstream:?})"
            );
        }
        let output = git_worktree_add(repo_path, target, None, local_branch)?;
        if !output.status.success() {
            bail!(combined_git_error(&output));
        }
    } else {
        let output = git_worktree_add(repo_path, target, Some(local_branch), remote_ref)?;
        if !output.status.success() {
            bail!(combined_git_error(&output));
        }
    }

    let output = git_command_output(
        target,
        ["branch", "--set-upstream-to", remote_ref, local_branch],
    )?;
    if !output.status.success() {
        bail!(combined_git_error(&output));
    }

    Ok(())
}

fn git_worktree_add(
    repo_path: &Path,
    target: &Path,
    new_branch: Option<&str>,
    start_point: &str,
) -> Result<Output> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo_path).args(["worktree", "add"]);
    if let Some(new_branch) = new_branch {
        command.args(["-b", new_branch]);
    }
    command
        .arg(target)
        .arg(start_point)
        .output()
        .context("failed to run git worktree add")
}

fn local_branch_exists(repo_path: &Path, branch: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch}");
    let output = git_command_output(repo_path, ["show-ref", "--verify", "--quiet", &ref_name])?;
    Ok(output.status.success())
}

fn needs_fallback_branch(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("already checked out")
        || message.contains("is already used by worktree")
        || message.contains("is checked out")
        || message.contains("does not track")
}

fn git_command_output<I, S>(path: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .context("failed to run git")
}

#[cfg(test)]
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
    fn branch_worktree_encoding_is_lossless_and_collision_free() {
        let branches = [
            "feature/API Demo",
            "feature-api-demo",
            "Feature/API Demo",
            "feature%2fAPI Demo",
            "release/naïve",
        ];
        let encoded = branches
            .iter()
            .map(|branch| branch_worktree_slug(branch).unwrap())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(encoded.len(), branches.len());
        for branch in branches {
            let encoded = branch_worktree_slug(branch).unwrap();
            assert_eq!(decode_branch_worktree_slug(&encoded).unwrap(), branch);
            assert!(
                encoded
                    .chars()
                    .all(|character| character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '-' | '/'))
            );
        }
    }

    #[test]
    fn branch_worktree_encoding_chunks_long_names_safely() {
        let branch = format!("feature/{}", "long-name".repeat(40));
        let encoded = branch_worktree_slug(&branch).unwrap();

        assert!(encoded.split('/').all(|component| component.len() <= 122));
        assert_eq!(decode_branch_worktree_slug(&encoded).unwrap(), branch);
    }

    #[test]
    fn parses_porcelain_v2_status_including_detached_head() {
        assert_eq!(
            parse_repo_status(
                "# branch.oid 0123456789abcdef\n# branch.head main\n? untracked.txt\n"
            ),
            Some(GitRepoStatus {
                branch: "main".to_string(),
                dirty: true,
            })
        );
        assert_eq!(
            parse_repo_status("# branch.oid fedcba9876543210\n# branch.head (detached)\n"),
            Some(GitRepoStatus {
                branch: "detached@fedcba9".to_string(),
                dirty: false,
            })
        );
        assert_eq!(parse_repo_status(""), None);
    }

    #[test]
    fn parses_worktree_porcelain_paths_with_spaces() {
        let records = parse_worktree_records(
            "worktree /collection/source repo\nHEAD abc123\nbranch refs/heads/main\n\nworktree /collection/work tree\nHEAD def456\nbranch refs/heads/feature/demo\n\n",
        );

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, PathBuf::from("/collection/source repo"));
        assert_eq!(records[0].local_branch.as_deref(), Some("main"));
        assert_eq!(records[1].path, PathBuf::from("/collection/work tree"));
        assert_eq!(records[1].local_branch.as_deref(), Some("feature/demo"));
    }

    #[cfg(unix)]
    #[test]
    fn repo_status_invokes_git_exactly_once() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let fake_git = temp.path().join("fake-git");
        let calls = temp.path().join("fake-git.calls");
        fs::write(
            &fake_git,
            "#!/bin/sh\necho \"$*\" >> \"$0.calls\"\nprintf '# branch.oid 0123456789abcdef\\n# branch.head main\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_git).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_git, permissions).unwrap();

        let status = repo_status_with_program(temp.path(), fake_git.as_os_str())
            .unwrap()
            .unwrap();

        assert_eq!(status.branch, "main");
        let calls = fs::read_to_string(calls).unwrap();
        assert_eq!(calls.lines().count(), 1);
        assert!(calls.contains("status --porcelain=v2 --branch"));
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
    fn nested_directory_is_not_mistaken_for_a_repository() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init", "-b", "main"]);
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();

        assert_eq!(repo_status(&child).unwrap(), None);
    }

    #[test]
    fn reports_git_branch_and_dirty_status() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init", "-b", "main"]);
        fs::write(temp.path().join("README.md"), "hello").unwrap();

        let status = repo_status(temp.path()).unwrap().unwrap();

        assert_eq!(status.branch, "main");
        assert!(status.dirty);
    }

    #[test]
    fn lists_origin_branches() {
        assert!(git_available(), "git is required to run this test");

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
        assert!(git_available(), "git is required to run this test");

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
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "feature/demo".to_string(),
            remote_ref: "origin/feature/demo".to_string(),
        };

        let outcome =
            ensure_worktree_from_origin(&repo, &temp.path().join("worktrees"), &branch).unwrap();
        let target = outcome.path;

        assert!(outcome.created);
        assert_eq!(outcome.source_repo, repo.canonicalize().unwrap());
        assert_eq!(outcome.remote_ref, "origin/feature/demo");
        assert_eq!(outcome.local_branch, "feature/demo");
        assert!(target.exists());
        assert_eq!(
            target,
            worktree_target_path(&repo, &temp.path().join("worktrees"), &branch)
                .unwrap()
                .canonicalize()
                .unwrap()
        );
        assert_eq!(
            git_output_text(&target, &["branch", "--show-current"]),
            "feature/demo"
        );
        let info = inspect_worktree(&target).unwrap().unwrap();
        assert_eq!(info.source_repo, repo.canonicalize().unwrap());
        assert_eq!(info.local_branch.as_deref(), Some("feature/demo"));
        assert_eq!(info.upstream.as_deref(), Some("origin/feature/demo"));

        let reused =
            ensure_worktree_from_origin(&repo, &temp.path().join("worktrees"), &branch).unwrap();
        assert!(!reused.created);
        assert_eq!(reused.path, target);
    }

    #[test]
    fn falls_back_when_local_branch_is_checked_out() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "main".to_string(),
            remote_ref: "origin/main".to_string(),
        };

        let outcome =
            ensure_worktree_from_origin(&repo, &temp.path().join("worktrees"), &branch).unwrap();
        let target = outcome.path;

        assert!(target.exists());
        assert!(outcome.created);
        assert_eq!(
            outcome.local_branch,
            git_output_text(&target, &["branch", "--show-current"])
        );
        assert!(outcome.local_branch.starts_with("dws/"));
    }

    #[test]
    fn refuses_to_reuse_target_from_another_source_repository() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        let (first_repo, _origin) = repo_with_origin(&first_root);
        let (second_repo, _origin) = repo_with_origin(&second_root);
        let branch = OriginBranch {
            name: "feature/demo".to_string(),
            remote_ref: "origin/feature/demo".to_string(),
        };
        let worktrees = temp.path().join("worktrees");
        ensure_worktree_from_origin(&first_repo, &worktrees, &branch).unwrap();

        let error = ensure_worktree_from_origin(&second_repo, &worktrees, &branch).unwrap_err();

        assert!(error.to_string().contains("belongs to"));
        assert!(
            worktree_target_path(&first_repo, &worktrees, &branch)
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn refuses_to_reuse_target_on_the_wrong_branch() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "feature/demo".to_string(),
            remote_ref: "origin/feature/demo".to_string(),
        };
        let worktrees = temp.path().join("worktrees");
        let outcome = ensure_worktree_from_origin(&repo, &worktrees, &branch).unwrap();
        run_git(&outcome.path, &["checkout", "-b", "unexpected"]);

        let error = ensure_worktree_from_origin(&repo, &worktrees, &branch).unwrap_err();

        assert!(error.to_string().contains("expected 'feature/demo'"));
        assert!(outcome.path.exists());
    }

    #[test]
    fn dirty_worktree_is_preserved_when_git_refuses_removal() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "feature/demo".to_string(),
            remote_ref: "origin/feature/demo".to_string(),
        };
        let outcome =
            ensure_worktree_from_origin(&repo, &temp.path().join("worktrees"), &branch).unwrap();
        fs::write(outcome.path.join("untracked.txt"), "keep me").unwrap();

        let error = remove_worktree(&outcome.path).unwrap_err();

        assert!(
            error
                .to_string()
                .contains(&outcome.path.display().to_string())
        );
        assert!(outcome.path.join("untracked.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_reuse_a_symlinked_target() {
        assert!(git_available(), "git is required to run this test");

        let temp = tempfile::tempdir().unwrap();
        let (repo, _origin) = repo_with_origin(temp.path());
        let branch = OriginBranch {
            name: "feature/demo".to_string(),
            remote_ref: "origin/feature/demo".to_string(),
        };
        let actual =
            ensure_worktree_from_origin(&repo, &temp.path().join("other"), &branch).unwrap();
        let worktrees = temp.path().join("worktrees");
        let target = worktree_target_path(&repo, &worktrees, &branch).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&actual.path, &target).unwrap();

        let error = ensure_worktree_from_origin(&repo, &worktrees, &branch).unwrap_err();

        assert!(error.to_string().contains("symlinked worktree target"));
        assert!(actual.path.exists());
    }
}
