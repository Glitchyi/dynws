use std::fs;
use std::path::Path;
use std::process::Command as Process;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;

fn dws(root: &Path) -> Command {
    let mut cmd = Command::cargo_bin("dws").unwrap();
    cmd.current_dir(root).env_remove("DYNWS_HOME");
    cmd
}
fn init(root: &Path) {
    dws(root).arg("init").assert().success();
}
fn git(root: &Path, args: &[&str]) -> String {
    let out = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn repo(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-b", "main"]);
    git(
        &path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    path
}
fn manifest(root: &Path, name: &str, repos: Value) -> std::path::PathBuf {
    let file = root.join("workspace.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!({"schema_version":1,"name":name,"repos":repos})).unwrap(),
    )
    .unwrap();
    file
}
fn create(root: &Path, file: &Path) -> Command {
    let mut cmd = dws(root);
    cmd.args(["create", "--file"]).arg(file);
    cmd
}
fn result(root: &Path, file: &Path) -> Value {
    let output = create(root, file)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn file_stdin_formats_paths_aliases_and_clean_output() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    fs::create_dir_all(root.join("defs/repo space")).unwrap();
    let file = manifest(
        &root.join("defs"),
        "links",
        json!([{"path":"repo space","kind":"link","name":"web"}]),
    );
    let first = result(root, &file);
    assert_eq!(first["status"], "created");
    let workspace = std::path::PathBuf::from(first["path"].as_str().unwrap());
    assert_eq!(
        fs::read_link(workspace.join("web")).unwrap(),
        root.join("defs/repo space").canonicalize().unwrap()
    );
    let toml_file = root.join("defs/workspace.toml");
    fs::write(&toml_file, "schema_version = 1\nname = 'links'\n[[repos]]\npath = 'repo space'\nkind = 'link'\nname = 'web'\n").unwrap();
    let out = create(root, &toml_file)
        .args(["--reuse", "--toml"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    assert_eq!(value["status"].as_str(), Some("reused"));
    assert_eq!(
        value["repos"][0]["path"].as_str(),
        first["repos"][0]["path"].as_str()
    );
    let stdin = json!({"schema_version":1,"name":"stdin","repos":[{"path":"defs/repo space","kind":"link"}]}).to_string();
    dws(root)
        .args(["create", "--file", "-", "--input-format", "json"])
        .write_stdin(stdin)
        .assert()
        .success()
        .stdout(format!(
            "{}\n",
            root.canonicalize()
                .unwrap()
                .join(".dynws/workspaces/stdin")
                .display()
        ));
}

#[test]
fn validation_and_project_precedence() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let collection = root.join("collection");
    fs::create_dir(&collection).unwrap();
    init(&collection);
    fs::create_dir(root.join("plain")).unwrap();
    let file = manifest(root, "valid", json!([{"path":"plain","kind":"link"}]));
    create(root, &file)
        .env("DYNWS_HOME", root.join("missing"))
        .arg("--project")
        .arg(&collection)
        .assert()
        .success();
    create(root, &file)
        .assert()
        .failure()
        .stderr(predicate::str::contains("no initialized"));
    dws(root)
        .args(["create", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--input-format"));
    dws(root)
        .args(["create", "--file", "-"])
        .arg("--project")
        .arg(&collection)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--input-format"));
    for repos in [
        json!([]),
        json!([{"path":"plain","kind":"link","branch":"main"}]),
        json!([{"path":"plain","kind":"worktree","branch":"main","base":"main"}]),
        json!([{"path":"plain","kind":"link","typo":true}]),
        json!([{"path":"plain","kind":"link"},{"path":"./plain","kind":"link","name":"other"}]),
    ] {
        let file = manifest(root, "invalid", repos);
        create(root, &file)
            .arg("--project")
            .arg(&collection)
            .assert()
            .failure();
        assert!(!collection.join(".dynws/sessions/invalid.toml").exists());
    }
    fs::write(&file, "{\"schema_version\":99,\"name\":\"x\",\"repos\":[]}").unwrap();
    create(root, &file)
        .arg("--project")
        .arg(&collection)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported manifest"));
}

#[test]
fn local_new_branches_reuse_after_commits_dirty_files_and_base_movement() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    let source = repo(root, "api");
    let initial = git(&source, &["rev-parse", "HEAD"]);
    let file = manifest(
        root,
        "fix",
        json!([{"path":"api","kind":"worktree","new_branch":"topic","base":"main"}]),
    );
    let first = result(root, &file);
    assert_eq!(first["repos"][0]["initial_commit"], initial);
    let target = Path::new(first["repos"][0]["path"].as_str().unwrap());
    fs::write(target.join("dirty"), "keep").unwrap();
    git(
        target,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "later",
        ],
    );
    git(
        &source,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "advance base",
        ],
    );
    let out = create(root, &file)
        .args(["--reuse", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let reused: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(reused["status"], "reused");
    assert_eq!(reused["repos"][0]["initial_commit"], initial);
    create(root, &file).assert().failure();
    fs::remove_file(root.join(".dynws/workspaces/fix/api")).unwrap();
    create(root, &file).arg("--reuse").assert().failure();
    assert!(target.join("dirty").exists());
}

#[test]
fn strict_existing_branch_and_remote_tracking_without_fallback() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    let source = repo(root, "api");
    let file = manifest(
        root,
        "occupied",
        json!([{"path":"api","kind":"worktree","branch":"main"}]),
    );
    create(root, &file)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already checked out"));
    git(&source, &["branch", "release"]);
    let file = manifest(
        root,
        "local",
        json!([{"path":"api","kind":"worktree","branch":"release"}]),
    );
    let local = result(root, &file);
    assert_eq!(local["repos"][0]["branch"], "release");
    // A local branch with a remote-looking name wins over a remote shorthand.
    git(&source, &["branch", "origin/topic"]);
    git(
        &source,
        &["update-ref", "refs/remotes/origin/topic", "HEAD"],
    );
    let file = manifest(
        root,
        "priority",
        json!([{"path":"api","kind":"worktree","branch":"origin/topic"}]),
    );
    assert_eq!(result(root, &file)["repos"][0]["branch"], "origin/topic");
    git(
        &source,
        &["remote", "add", "origin", source.to_str().unwrap()],
    );
    git(
        &source,
        &["update-ref", "refs/remotes/origin/remote", "HEAD"],
    );
    let file = manifest(
        root,
        "remote",
        json!([{"path":"api","kind":"worktree","branch":"refs/remotes/origin/remote"}]),
    );
    let remote = result(root, &file);
    assert_eq!(remote["repos"][0]["branch"], "remote");
    assert_eq!(
        git(
            Path::new(remote["repos"][0]["path"].as_str().unwrap()),
            &["rev-parse", "--abbrev-ref", "@{upstream}"]
        ),
        "origin/remote"
    );
}

#[test]
fn reuse_rejects_intent_and_symlink_tampering_and_legacy_adoption() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    fs::create_dir(root.join("one")).unwrap();
    fs::create_dir(root.join("two")).unwrap();
    let file = manifest(root, "links", json!([{"path":"one","kind":"link"}]));
    result(root, &file);
    let changed = manifest(
        root,
        "links",
        json!([{"path":"two","kind":"link","name":"one"}]),
    );
    create(root, &changed)
        .arg("--reuse")
        .assert()
        .failure()
        .stderr(predicate::str::contains("intent"));
    let file = manifest(root, "links", json!([{"path":"one","kind":"link"}]));
    let workspace = root.join(".dynws/workspaces/links");
    fs::remove_file(workspace.join("one")).unwrap();
    std::os::unix::fs::symlink(root.join("two"), workspace.join("one")).unwrap();
    create(root, &file).arg("--reuse").assert().failure();
    fs::remove_file(workspace.join("one")).unwrap();
    std::os::unix::fs::symlink(
        root.join("one").canonicalize().unwrap(),
        workspace.join("one"),
    )
    .unwrap();
    fs::write(workspace.join("unexpected"), "keep").unwrap();
    create(root, &file).arg("--reuse").assert().failure();
    fs::remove_file(workspace.join("unexpected")).unwrap();
    let metadata = root.join(".dynws/sessions/links.toml");
    let mut value: toml::Value = toml::from_str(&fs::read_to_string(&metadata).unwrap()).unwrap();
    value.as_table_mut().unwrap().remove("creation");
    fs::write(metadata, toml::to_string(&value).unwrap()).unwrap();
    create(root, &file)
        .arg("--reuse")
        .assert()
        .failure()
        .stderr(predicate::str::contains("legacy"));
}

fn git_wrapper(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let real = Process::new("which").arg("git").output().unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let script = bin.join("git");
    fs::write(&script, r#"#!/bin/sh
if [ "$3" = fetch ]; then
  printf '%s\n' "$4" >> "$DWS_TEST_FETCH_LOG"
  if [ "$DWS_TEST_MODE" = auth ]; then
    [ "$GIT_TERMINAL_PROMPT" = 0 ] || exit 88
    [ "$GIT_ASKPASS" = /usr/bin/false ] || exit 89
    [ "$SSH_ASKPASS" = /usr/bin/false ] || exit 90
    [ "$GIT_SSH_COMMAND" = 'ssh -o BatchMode=yes -o StrictHostKeyChecking=yes' ] || exit 91
    echo 'authentication denied without prompting' >&2
    exit 1
  fi
fi
if [ "$3" = worktree ] && [ "$4" = add ]; then
  if [ "$DWS_TEST_MODE" = second ]; then
    if ! mkdir "$DWS_TEST_MARKER" 2>/dev/null; then echo 'injected second add failure' >&2; exit 1; fi
  fi
  "$DWS_TEST_REAL_GIT" "$@" || exit $?
  if [ "$DWS_TEST_MODE" = publication ]; then
    mkdir "$DWS_TEST_WORKSPACE"
    printf keep > "$DWS_TEST_WORKSPACE/keep"
  fi
  if [ "$DWS_TEST_MODE" = dirty ]; then
    if ! mkdir "$DWS_TEST_MARKER" 2>/dev/null; then echo 'injected second add failure' >&2; exit 1; fi
    for path in "$DWS_TEST_WORKTREES"/strict-*; do printf keep > "$path/dirty"; done
  fi
  if [ "$DWS_TEST_MODE" = link ]; then
    for path in "$DWS_TEST_WORKSPACES"/.*.create; do printf keep > "$path/api"; done
  fi
  exit 0
fi
exec "$DWS_TEST_REAL_GIT" "$@"
"#).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("real-git"), real).unwrap();
    bin
}
fn wrapped(root: &Path, file: &Path, bin: &Path, mode: &str) -> Command {
    let mut cmd = create(root, file);
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    cmd.env("PATH", std::env::join_paths(paths).unwrap())
        .env(
            "DWS_TEST_REAL_GIT",
            fs::read_to_string(root.join("real-git")).unwrap(),
        )
        .env("DWS_TEST_MODE", mode)
        .env("DWS_TEST_MARKER", root.join("attempted"))
        .env("DWS_TEST_FETCH_LOG", root.join("fetch.log"))
        .env("DWS_TEST_WORKSPACE", root.join(".dynws/workspaces/fail"))
        .env("DWS_TEST_WORKTREES", root.join(".dynws/worktrees"))
        .env("DWS_TEST_WORKSPACES", root.join(".dynws/workspaces"));
    cmd
}

#[test]
fn rollback_removes_created_worktrees_retains_branches_and_prevents_adoption() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    let a = repo(root, "api");
    let b = repo(root, "worker");
    let bin = git_wrapper(root);
    let file = manifest(
        root,
        "fail",
        json!([{"path":"api","kind":"worktree","new_branch":"fix","base":"main"},{"path":"worker","kind":"worktree","new_branch":"fix","base":"main"}]),
    );
    wrapped(root, &file, &bin, "second")
        .assert()
        .failure()
        .stderr(predicate::str::contains("retained"));
    assert!(!root.join(".dynws/sessions/fail.toml").exists());
    assert!(!root.join(".dynws/workspaces/fail").exists());
    assert_eq!(git(&a, &["branch", "--list", "fix"]), "fix");
    assert_eq!(git(&b, &["branch", "--list", "fix"]), "");
    assert_eq!(
        fs::read_dir(root.join(".dynws/worktrees")).unwrap().count(),
        0
    );
    create(root, &file)
        .arg("--reuse")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn rollback_preserves_dirty_worktrees_and_unexpected_publication_files() {
    for mode in ["dirty", "publication", "link"] {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        init(root);
        repo(root, "api");
        repo(root, "worker");
        let bin = git_wrapper(root);
        let repos = if mode == "dirty" {
            json!([{"path":"api","kind":"worktree","new_branch":"fix","base":"main"},{"path":"worker","kind":"worktree","new_branch":"fix","base":"main"}])
        } else {
            json!([{"path":"api","kind":"worktree","new_branch":"fix","base":"main"}])
        };
        let file = manifest(root, "fail", repos);
        wrapped(root, &file, &bin, mode).assert().failure();
        assert!(!root.join(".dynws/sessions/fail.toml").exists());
        if mode == "dirty" {
            assert!(
                fs::read_dir(root.join(".dynws/worktrees"))
                    .unwrap()
                    .any(|p| p.unwrap().path().join("dirty").exists())
            );
        } else if mode == "publication" {
            assert_eq!(
                fs::read_to_string(root.join(".dynws/workspaces/fail/keep")).unwrap(),
                "keep"
            );
        } else {
            assert!(
                fs::read_dir(root.join(".dynws/workspaces"))
                    .unwrap()
                    .any(|p| p.unwrap().path().join("api").is_file())
            );
        }
    }
}

#[test]
fn fetch_is_origin_only_once_per_source_skips_links_and_disables_prompts() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    let source = repo(root, "api");
    git(
        &source,
        &["remote", "add", "origin", source.to_str().unwrap()],
    );
    git(&source, &["branch", "other"]);
    let external = root.join("external");
    git(
        &source,
        &["worktree", "add", external.to_str().unwrap(), "other"],
    );
    let bin = git_wrapper(root);
    fs::create_dir(root.join("plain")).unwrap();
    let file = manifest(
        root,
        "fetched",
        json!([{"path":"api","kind":"worktree","new_branch":"one","base":"main"},{"path":"external","kind":"worktree","new_branch":"two","base":"main"},{"path":"plain","kind":"link"}]),
    );
    wrapped(root, &file, &bin, "ok")
        .arg("--fetch")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(root.join("fetch.log")).unwrap(),
        "origin\n"
    );
    fs::remove_file(root.join("fetch.log")).unwrap();
    let file = manifest(
        root,
        "auth",
        json!([{"path":"api","kind":"worktree","new_branch":"three","base":"main"}]),
    );
    wrapped(root, &file, &bin, "auth")
        .arg("--fetch")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "authentication denied without prompting",
        ));
    assert!(!root.join(".dynws/sessions/auth.toml").exists());
    assert_eq!(git(&source, &["branch", "--list", "three"]), "");
}

#[test]
fn collection_lock_prevents_concurrent_creation_without_mutation() {
    use fs2::FileExt;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    fs::create_dir(root.join("plain")).unwrap();
    let file = manifest(root, "locked", json!([{"path":"plain","kind":"link"}]));
    let lock = fs::File::create(root.join(".dynws/create.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    create(root, &file)
        .assert()
        .failure()
        .stderr(predicate::str::contains("another session mutation"));
    assert!(!root.join(".dynws/workspaces/locked").exists());
    drop(lock);
    create(root, &file).assert().success();
}

#[test]
fn quoted_source_paths_and_symlink_inputs_preserve_git_identity() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    let source = repo(root, "api \"quoted\"\nline");
    std::os::unix::fs::symlink(&source, root.join("alias")).unwrap();
    let file = manifest(
        root,
        "quoted",
        json!([{"path":"alias","name":"api","kind":"worktree","new_branch":"topic","base":"main"}]),
    );
    result(root, &file);
    create(root, &file).arg("--reuse").assert().success();
}

#[test]
fn mixed_reordered_input_formats_produce_identical_reuse_results_without_editor_launch() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    repo(root, "api");
    fs::create_dir(root.join("web")).unwrap();
    // An unusable editor configuration must be irrelevant to noninteractive creation.
    fs::write(
        root.join(".dynws/config.toml"),
        "[editor]\ndefault = '/does-not-exist/editor'\n",
    )
    .unwrap();
    let file = manifest(
        root,
        "mixed",
        json!([{"path":"web","kind":"link","name":"frontend"},{"path":"api","kind":"worktree","new_branch":"fix","base":"main"}]),
    );
    result(root, &file);
    let out = create(root, &file)
        .args(["--reuse", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&out).unwrap();
    let toml_file = root.join("workspace.toml");
    fs::write(&toml_file,"schema_version=1\nname='mixed'\n[[repos]]\npath='api'\nkind='worktree'\nnew_branch='fix'\nbase='main'\n[[repos]]\npath='web'\nname='frontend'\nkind='link'\n").unwrap();
    let out = create(root, &toml_file)
        .args(["--reuse", "--toml"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let toml: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    assert_eq!(json, serde_json::to_value(toml).unwrap());
    create(root, &file)
        .args(["--reuse", "--json", "--toml"])
        .assert()
        .failure();
    let file = manifest(
        root,
        "mixed",
        json!([{"path":"web","kind":"link","name":"frontend"},{"path":"api","kind":"worktree","new_branch":"fix","base":"HEAD"}]),
    );
    create(root, &file)
        .arg("--reuse")
        .assert()
        .failure()
        .stderr(predicate::str::contains("intent"));
}

#[test]
fn remote_branch_rejects_wrong_upstream_and_other_remote_fetch_is_not_implied() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    init(root);
    let source = repo(root, "api");
    git(
        &source,
        &["remote", "add", "upstream", source.to_str().unwrap()],
    );
    git(
        &source,
        &["update-ref", "refs/remotes/upstream/release", "HEAD"],
    );
    git(&source, &["branch", "release"]);
    let file = manifest(
        root,
        "remote",
        json!([{"path":"api","kind":"worktree","branch":"refs/remotes/upstream/release"}]),
    );
    create(root, &file)
        .assert()
        .failure()
        .stderr(predicate::str::contains("must track exactly"));
    git(
        &source,
        &["branch", "--set-upstream-to=upstream/release", "release"],
    );
    // Works entirely offline without an origin remote.
    let created = result(root, &file);
    assert_eq!(created["repos"][0]["branch"], "release");
    create(root, &file).arg("--reuse").assert().success();
    // --fetch is specifically origin-only and fails if origin is absent.
    create(root, &file)
        .args(["--reuse", "--fetch"])
        .assert()
        .failure();
}
