use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn list_uses_dynws_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("demo.toml"),
        r#"
name = "demo"
repos = [{ name = "repo-a", path = "/tmp/repo-a" }]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("demo"))
        .stdout(predicate::str::contains("repo-a -> /tmp/repo-a"));
}

#[test]
fn path_prints_session_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("demo.toml"),
        r#"
name = "demo"
repos = [{ name = "repo-a", path = "/tmp/repo-a" }]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .args(["path", "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            home.join("workspaces/demo").display().to_string(),
        ));
}

#[test]
fn default_home_is_local_dynws_folder() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join(".dynws/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("local.toml"),
        r#"
name = "local"
repos = [{ name = "repo-a", path = "/tmp/repo-a" }]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();

    Command::cargo_bin("dws")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("DYNWS_HOME")
        .args(["list", "--plain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("local"))
        .stdout(predicate::str::contains("repo-a -> /tmp/repo-a"));
}

#[test]
fn init_zsh_prints_cd_and_zoxide_helpers() {
    Command::cargo_bin("dws")
        .unwrap()
        .args(["init", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dws-cd()"))
        .stdout(predicate::str::contains("dws-z()"))
        .stdout(predicate::str::contains("zoxide add"));
}

#[cfg(unix)]
#[test]
fn zoxide_sync_adds_existing_workspaces() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let sessions = home.join("sessions");
    let workspace = home.join("workspaces/demo");
    let fake_bin = temp.path().join("bin");
    let log = temp.path().join("zoxide.log");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(
        sessions.join("demo.toml"),
        r#"
name = "demo"
repos = [{ name = "repo-a", path = "/tmp/repo-a" }]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();

    let fake_zoxide = fake_bin.join("zoxide");
    fs::write(
        &fake_zoxide,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "zoxide 0.0.0"
  exit 0
fi
echo "$@" >> "$DWS_TEST_ZOXIDE_LOG"
exit 0
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_zoxide).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_zoxide, permissions).unwrap();

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .env("PATH", path)
        .env("DWS_TEST_ZOXIDE_LOG", &log)
        .args(["zoxide", "sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("synced 1 workspace"));

    let logged = fs::read_to_string(log).unwrap();
    assert!(logged.contains(&format!("add {}", workspace.display())));
}

#[test]
fn empty_list_is_successful() {
    let temp = tempfile::tempdir().unwrap();

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", temp.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("no sessions found"));
}
