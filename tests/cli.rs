use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use assert_cmd::Command;
use predicates::prelude::*;

fn dws() -> Command {
    Command::cargo_bin("dws").unwrap()
}

fn init(root: &std::path::Path) {
    dws()
        .current_dir(root)
        .env_remove("DYNWS_HOME")
        .arg("init")
        .assert()
        .success();
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path) {
    fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn init_creates_versioned_project_layout() {
    let temp = tempfile::tempdir().unwrap();
    let canonical_root = temp.path().canonicalize().unwrap();

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized dws project"))
        .stdout(predicate::str::contains(
            canonical_root.display().to_string(),
        ));

    let home = temp.path().join(".dynws");
    let marker = fs::read_to_string(home.join("project.toml")).unwrap();
    let marker: toml::Value = toml::from_str(&marker).unwrap();
    assert_eq!(marker["schema_version"].as_integer(), Some(1));
    assert_eq!(marker["collection_root"].as_str(), canonical_root.to_str());
    assert!(home.join("sessions").is_dir());
    assert!(home.join("workspaces").is_dir());
    assert!(home.join("worktrees").is_dir());
    assert!(!home.join("config.toml").exists());
}

#[test]
fn init_is_idempotent_and_repairs_only_managed_layout() {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    let home = temp.path().join(".dynws");
    let marker_before = fs::read_to_string(home.join("project.toml")).unwrap();
    fs::write(home.join("keep.txt"), "user data").unwrap();
    fs::remove_dir(home.join("worktrees")).unwrap();

    init(temp.path());

    assert_eq!(
        fs::read_to_string(home.join("project.toml")).unwrap(),
        marker_before
    );
    assert_eq!(
        fs::read_to_string(home.join("keep.txt")).unwrap(),
        "user data"
    );
    assert!(home.join("worktrees").is_dir());
}

#[test]
fn init_adopts_a_valid_markerless_layout() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".dynws");
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::create_dir_all(home.join("workspaces")).unwrap();
    fs::create_dir_all(home.join("worktrees")).unwrap();
    fs::write(home.join("config.toml"), "[editor]\ndefault = \"code\"\n").unwrap();

    init(temp.path());

    assert!(home.join("project.toml").is_file());
    assert!(
        fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .contains("code")
    );
}

#[cfg(unix)]
#[test]
fn init_adopts_valid_legacy_session_links_and_rejects_wrong_targets() {
    use std::os::unix::fs::symlink;

    let valid = tempfile::tempdir().unwrap();
    let valid_home = valid.path().join(".dynws");
    let valid_repo = valid.path().join("repo");
    fs::create_dir(&valid_repo).unwrap();
    fs::create_dir_all(valid_home.join("sessions")).unwrap();
    fs::create_dir_all(valid_home.join("workspaces/demo")).unwrap();
    fs::create_dir_all(valid_home.join("worktrees")).unwrap();
    fs::write(
        valid_home.join("sessions/demo.toml"),
        format!(
            r#"name = "demo"
repos = [{{ name = "repo", path = {:?} }}]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
            valid_repo.canonicalize().unwrap().display().to_string()
        ),
    )
    .unwrap();
    symlink(
        valid_repo.canonicalize().unwrap(),
        valid_home.join("workspaces/demo/repo"),
    )
    .unwrap();

    init(valid.path());
    assert!(valid_home.join("project.toml").is_file());

    let invalid = tempfile::tempdir().unwrap();
    let invalid_home = invalid.path().join(".dynws");
    let expected_repo = invalid.path().join("expected");
    let wrong_repo = invalid.path().join("wrong");
    fs::create_dir(&expected_repo).unwrap();
    fs::create_dir(&wrong_repo).unwrap();
    fs::create_dir_all(invalid_home.join("sessions")).unwrap();
    fs::create_dir_all(invalid_home.join("workspaces/demo")).unwrap();
    fs::create_dir_all(invalid_home.join("worktrees")).unwrap();
    fs::write(
        invalid_home.join("sessions/demo.toml"),
        format!(
            r#"name = "demo"
repos = [{{ name = "repo", path = {:?} }}]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
            expected_repo.canonicalize().unwrap().display().to_string()
        ),
    )
    .unwrap();
    symlink(
        wrong_repo.canonicalize().unwrap(),
        invalid_home.join("workspaces/demo/repo"),
    )
    .unwrap();

    dws()
        .current_dir(invalid.path())
        .env_remove("DYNWS_HOME")
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("link target does not match"));
    assert!(!invalid_home.join("project.toml").exists());
}

#[test]
fn invalid_legacy_layout_is_rejected_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".dynws");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("unknown.data"), "sentinel").unwrap();

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected entry"));

    assert_eq!(
        fs::read_to_string(home.join("unknown.data")).unwrap(),
        "sentinel"
    );
    assert!(!home.join("project.toml").exists());
    assert!(!home.join("sessions").exists());
}

#[test]
fn operational_commands_find_the_nearest_initialized_ancestor() {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    let nested = temp.path().join("repos/service/src");
    fs::create_dir_all(&nested).unwrap();

    dws()
        .current_dir(&nested)
        .env_remove("DYNWS_HOME")
        .args(["config", "clear", "editor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already unset"));
}

#[test]
fn dynws_home_is_exact_and_uses_its_recorded_collection_root() {
    let temp = tempfile::tempdir().unwrap();
    let collection = temp.path().join("collection");
    let elsewhere = temp.path().join("elsewhere");
    let home = temp.path().join("state/dynws-home");
    fs::create_dir_all(&collection).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();

    dws()
        .current_dir(&collection)
        .env("DYNWS_HOME", &home)
        .arg("init")
        .assert()
        .success();

    dws()
        .current_dir(&elsewhere)
        .env("DYNWS_HOME", &home)
        .args(["config", "clear", "editor"])
        .assert()
        .success();

    let marker: toml::Value =
        toml::from_str(&fs::read_to_string(home.join("project.toml")).unwrap()).unwrap();
    assert_eq!(
        marker["collection_root"].as_str(),
        collection.canonicalize().unwrap().to_str()
    );

    let missing_home = temp.path().join("not-initialized");
    dws()
        .current_dir(&collection)
        .env("DYNWS_HOME", &missing_home)
        .args(["config", "clear", "editor"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "DYNWS_HOME does not point to an initialized dws project",
        ));
    assert!(!missing_home.exists());
}

#[test]
fn every_operational_entrypoint_is_guarded_and_side_effect_free() {
    let invocations: &[&[&str]] = &[
        &[],
        &["new", "--no-open"],
        &["new", "--editor", "missing-editor"],
        &["open", "missing"],
        &["new"],
        &["edit", "missing", "--name", "new"],
        &["remove", "missing"],
        &["reveal", "missing"],
        &["config", "list", "file-manager"],
        &["config", "set", "editor", "missing-editor"],
        &["config", "clear", "editor"],
        &["config", "list", "file-manager"],
        &["config", "set", "file-manager", "missing-manager"],
        &["config", "clear", "file-manager"],
    ];

    for args in invocations {
        let temp = tempfile::tempdir().unwrap();
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(*args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("run 'dws init'"));
        assert!(
            !temp.path().join(".dynws").exists(),
            "command unexpectedly initialized storage: {args:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn initialized_config_commands_persist_without_changing_the_layout() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(&bin.join("fake-editor"));
    write_executable(&bin.join("fake-manager"));
    init(temp.path());
    let marker_before = fs::read_to_string(temp.path().join(".dynws/project.toml")).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .env("PATH", &path)
        .args(["config", "set", "editor", "fake-editor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("default editor set"));
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .env("PATH", &path)
        .args(["config", "set", "file-manager", "fake-manager"])
        .assert()
        .success()
        .stdout(predicate::str::contains("default file manager set"));

    let config = fs::read_to_string(temp.path().join(".dynws/config.toml")).unwrap();
    assert!(config.contains("fake-editor"));
    assert!(config.contains("fake-manager"));
    assert_eq!(
        fs::read_to_string(temp.path().join(".dynws/project.toml")).unwrap(),
        marker_before
    );
}

#[test]
fn edit_uses_one_combined_update() {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    let home = temp.path().join(".dynws");
    fs::create_dir(home.join("workspaces/demo")).unwrap();
    fs::write(
        home.join("sessions/demo.toml"),
        r#"name = "demo"
description = "old"
repos = []
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args([
            "edit",
            "demo",
            "--name",
            "renamed",
            "--description",
            "new description",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated session demo -> renamed"));

    assert!(!home.join("sessions/demo.toml").exists());
    assert!(!home.join("workspaces/demo").exists());
    assert!(home.join("workspaces/renamed").is_dir());
    let metadata = fs::read_to_string(home.join("sessions/renamed.toml")).unwrap();
    assert!(metadata.contains("name = \"renamed\""));
    assert!(metadata.contains("description = \"new description\""));
}

#[test]
fn help_and_version_work_without_initialization() {
    let temp = tempfile::tempdir().unwrap();

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: dws"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("open"))
        .stdout(predicate::str::contains("new"))
        .stdout(predicate::str::contains("config"))
        .stdout(predicate::str::contains("setup").not())
        .stdout(predicate::str::contains("zoxide").not());

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("dws 0.1.0"));

    assert!(!temp.path().join(".dynws").exists());
}

#[test]
fn ancestor_marker_must_belong_to_its_directory() {
    let temp = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let home = temp.path().join(".dynws");
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::create_dir_all(home.join("workspaces")).unwrap();
    fs::create_dir_all(home.join("worktrees")).unwrap();
    fs::write(
        home.join("project.toml"),
        format!(
            "schema_version = 1\ncollection_root = {:?}\n",
            unrelated
                .path()
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        ),
    )
    .unwrap();

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["config", "list", "file-manager"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("records collection root"));
}

#[cfg(unix)]
#[test]
fn dangling_inner_marker_does_not_fall_through_to_an_outer_project() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    let nested = temp.path().join("nested");
    fs::create_dir_all(nested.join(".dynws")).unwrap();
    symlink(
        nested.join("missing-marker"),
        nested.join(".dynws/project.toml"),
    )
    .unwrap();

    dws()
        .current_dir(&nested)
        .env_remove("DYNWS_HOME")
        .args(["config", "clear", "editor"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "project marker is not a regular file",
        ));
}

#[test]
fn removed_commands_and_shell_init_are_rejected_by_clap() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["setup"],
        vec!["manage"],
        vec!["config", "editor", "list"],
        vec!["config", "list", "editor"],
        vec!["--no-open"],
        vec!["list"],
        vec!["path", "demo"],
        vec!["zoxide", "sync"],
        vec!["init", "zsh"],
    ] {
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(&args)
            .assert()
            .failure()
            .stderr(
                predicate::str::contains("unexpected argument")
                    .or(predicate::str::contains("unrecognized subcommand"))
                    .or(predicate::str::contains("invalid value")),
            );
    }
    assert!(!temp.path().join(".dynws").exists());
}

#[test]
fn incomplete_or_unsupported_projects_fail_without_repairing() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".dynws");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("project.toml"),
        format!(
            "schema_version = 99\ncollection_root = {:?}\n",
            temp.path().canonicalize().unwrap().display().to_string()
        ),
    )
    .unwrap();

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["config", "list", "file-manager"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unsupported dws project schema 99",
        ));

    assert!(!home.join("sessions").exists());
    assert!(!home.join("workspaces").exists());
    assert!(!home.join("worktrees").exists());
}

#[test]
fn interactive_entrypoints_reject_pipes_and_conflicts_before_terminal_setup() {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    for args in [vec![], vec!["new"], vec!["new", "--no-open"]] {
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("require a terminal"));
    }
    for args in [
        vec!["new", "--editor", "sh", "--no-open"],
        vec!["edit", "demo", "--description", "x", "--clear-description"],
        vec!["--editor", "sh", "open", "demo"],
    ] {
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(args)
            .assert()
            .code(2);
    }
}

#[cfg(unix)]
#[test]
fn editor_is_waited_for_preserves_quoted_arguments_and_reports_failure() {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    let home = temp.path().join(".dynws");
    fs::create_dir(home.join("workspaces/demo")).unwrap();
    fs::write(
        home.join("sessions/demo.toml"),
        "name='demo'\nrepos=[]\ncreated_at='now'\nupdated_at='now'\n",
    )
    .unwrap();
    let marker = temp.path().join("finished");
    let script = temp.path().join("editor with spaces");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nsleep 0.05\nprintf done > '{}'\n",
            temp.path().join("args").display(),
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let command = format!("\"{}\" \"two words\" '$(literal)'", script.display());
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["open", "demo", "--editor", &command])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(marker).unwrap(), "done");
    let args = fs::read_to_string(temp.path().join("args")).unwrap();
    assert!(args.starts_with("two words\n$(literal)\n"));
    assert!(args.ends_with(
        &format!("{}\n", home.join("workspaces/demo").canonicalize().unwrap().display())
    ));
    fs::write(&script, "#!/bin/sh\nexit 7\n").unwrap();
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["open", "demo", "--editor", &command])
        .assert()
        .failure()
        .stderr(predicate::str::contains("editor exited"));
    assert!(home.join("sessions/demo.toml").is_file());
}

#[test]
fn live_completion_is_scoped_fresh_quiet_and_never_runs_git_or_writes() {
    let temp = tempfile::tempdir().unwrap();
    init(temp.path());
    let home = temp.path().join(".dynws");
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    #[cfg(unix)]
    {
        fs::write(
            bin.join("git"),
            "#!/bin/sh\necho 'completion must not invoke git' >&2\nexit 99\n",
        )
        .unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let write = |name: &str| {
        fs::write(home.join("sessions").join(format!("{name}.toml")), format!(
        "name='{name}'\ncreated_at='now'\nupdated_at='now'\n[[repos]]\nname='api'\npath='/missing-worktree'\nkind='worktree'\nsource_repo='/missing-source'\nlocal_branch='main'\n"
    )).unwrap()
    };
    write("api-review");
    fs::write(home.join("sessions/bad.toml"), "bad").unwrap();
    let complete = |cwd: &std::path::Path, explicit: Option<&std::path::Path>| {
        let mut cmd = dws();
        cmd.current_dir(cwd)
            .env("DWS_COMPLETE", "zsh")
            .env("_CLAP_COMPLETE_INDEX", "2")
            .env("_CLAP_IFS", "\n")
            .env("PATH", &bin);
        if let Some(home) = explicit {
            cmd.env("DYNWS_HOME", home);
        } else {
            cmd.env_remove("DYNWS_HOME");
        }
        cmd.args(["--", "dws", "open", "api-"])
            .assert()
            .success()
            .stderr("")
            .get_output()
            .stdout
            .clone()
    };
    assert!(
        String::from_utf8(complete(&nested, None))
            .unwrap()
            .contains("api-review")
    );
    assert!(!home.join("create.lock").exists());
    fs::remove_file(home.join("sessions/api-review.toml")).unwrap();
    write("api-renamed");
    let output = String::from_utf8(complete(&nested, None)).unwrap();
    assert!(output.contains("api-renamed") && !output.contains("api-review"));
    let other = tempfile::tempdir().unwrap();
    init(other.path());
    assert!(complete(other.path(), None).is_empty());
    assert!(
        String::from_utf8(complete(other.path(), Some(&home)))
            .unwrap()
            .contains("api-renamed")
    );
    let absent = tempfile::tempdir().unwrap();
    assert!(complete(absent.path(), None).is_empty());
    assert!(!absent.path().join(".dynws").exists());
    assert!(!home.join("create.lock").exists());
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["open", "api-renamed", "--editor", "sh"])
        .assert()
        .failure();
}

#[cfg(unix)]
fn session_with_slow_editor(temp: &tempfile::TempDir) -> (std::path::PathBuf, String) {
    init(temp.path());
    let home = temp.path().join(".dynws");
    fs::create_dir(home.join("workspaces/demo")).unwrap();
    fs::write(
        home.join("sessions/demo.toml"),
        "name='demo'\nrepos=[]\ncreated_at='now'\nupdated_at='now'\n",
    )
    .unwrap();
    let marker = temp.path().join("editor-finished");
    let script = temp.path().join("slow-editor");
    fs::write(
        &script,
        format!("#!/bin/sh\nsleep 2\nprintf done > '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (marker, format!("\"{}\"", script.display()))
}

#[cfg(unix)]
#[test]
fn background_open_returns_at_once_and_hands_the_workspace_to_the_shell() {
    let temp = tempfile::tempdir().unwrap();
    let (marker, editor) = session_with_slow_editor(&temp);
    for (key, value, said) in [
        ("background", "on", "background open on"),
        ("cd", "yes", "cd into workspace on"),
    ] {
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(["config", "set", key, value])
            .assert()
            .success()
            .stdout(predicate::str::contains(said));
    }
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["config", "set", "cd", "sometimes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected on or off"));

    let cd_file = temp.path().join("cd-target");
    let started = std::time::Instant::now();
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .env("DWS_CD_FILE", &cd_file)
        .args(["open", "demo", "--editor", &editor])
        .assert()
        .success()
        .stderr(predicate::str::contains("in the background"));
    assert!(started.elapsed() < std::time::Duration::from_millis(1500));
    assert!(!marker.exists(), "dws waited for the editor");
    let target = std::path::PathBuf::from(fs::read_to_string(&cd_file).unwrap());
    assert_eq!(
        target.canonicalize().unwrap(),
        temp.path()
            .join(".dynws/workspaces/demo")
            .canonicalize()
            .unwrap()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !marker.exists() {
        assert!(std::time::Instant::now() < deadline, "the editor never ran");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .env_remove("DWS_CD_FILE")
        .args(["open", "demo", "--editor", &editor])
        .assert()
        .success()
        .stderr(predicate::str::contains("dws shell-init"));
    dws()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["config", "clear", "background"])
        .assert()
        .success()
        .stdout(predicate::str::contains("background open off"));
    let config = fs::read_to_string(temp.path().join(".dynws/config.toml")).unwrap();
    assert!(config.contains("cd = true") && !config.contains("background"));
}

#[test]
fn shell_init_prints_a_cd_function_and_completion_without_a_project() {
    let temp = tempfile::tempdir().unwrap();
    for (shell, completion) in [
        ("zsh", "source <(DWS_COMPLETE=zsh command dws)"),
        ("bash", "eval \"$(DWS_COMPLETE=bash command dws)\""),
    ] {
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(["shell-init", shell])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("dws() {")
                    .and(predicate::str::contains("DWS_CD_FILE"))
                    .and(predicate::str::contains("cd -- \"$dws_target\""))
                    .and(predicate::str::contains(completion)),
            );
    }
    assert!(!temp.path().join(".dynws").exists());
}

#[cfg(unix)]
#[test]
fn shell_function_moves_bash_into_the_opened_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let (_, editor) = session_with_slow_editor(&temp);
    for key in ["background", "cd"] {
        dws()
            .current_dir(temp.path())
            .env_remove("DYNWS_HOME")
            .args(["config", "set", key, "on"])
            .assert()
            .success();
    }
    let binary = assert_cmd::cargo::cargo_bin("dws");
    let path = format!(
        "{}:{}",
        binary.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new("bash")
        .arg("--noprofile")
        .arg("--norc")
        .arg("-c")
        .arg(format!(
            "eval \"$(dws shell-init bash)\" && dws open demo --editor '{}' 2>/dev/null && pwd -P",
            editor.replace('\'', "'\\''")
        ))
        .current_dir(temp.path())
        .env("PATH", path)
        .env_remove("DYNWS_HOME")
        .env_remove("DWS_CD_FILE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        temp.path()
            .join(".dynws/workspaces/demo")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
}
