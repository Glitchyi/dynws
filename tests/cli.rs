use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

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

#[test]
fn setup_requires_yes_without_tty() {
    let temp = tempfile::tempdir().unwrap();

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", temp.path().join("dynws-home"))
        .arg("setup")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "dws setup requires a terminal; pass --yes",
        ));
}

#[test]
fn setup_yes_creates_project_layout_and_config() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized dws project"));

    assert!(home.join("config.toml").exists());
    assert!(home.join("sessions").is_dir());
    assert!(home.join("workspaces").is_dir());
    assert!(home.join("worktrees").is_dir());
}

#[cfg(unix)]
#[test]
fn setup_yes_writes_editor_and_file_manager_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    write_executable(&fake_bin.join("fake-editor"), "#!/bin/sh\nexit 0\n");
    write_executable(&fake_bin.join("fake-open"), "#!/bin/sh\nexit 0\n");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .env("PATH", path)
        .args([
            "setup",
            "--yes",
            "--editor",
            "fake-editor",
            "--file-manager",
            "fake-open --reveal",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("default editor: fake-editor"))
        .stdout(predicate::str::contains(
            "default file manager: fake-open --reveal",
        ));

    let config = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("[editor]"));
    assert!(config.contains("default = \"fake-editor\""));
    assert!(config.contains("[file_manager]"));
    assert!(config.contains("default = \"fake-open --reveal\""));
}

#[cfg(unix)]
#[test]
fn zoxide_sync_adds_existing_workspaces() {
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
    write_executable(
        &fake_zoxide,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "zoxide 0.0.0"
  exit 0
fi
echo "$@" >> "$DWS_TEST_ZOXIDE_LOG"
exit 0
"#,
    );

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

#[test]
fn manage_edit_renames_session() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let sessions = home.join("sessions");
    let workspace = home.join("workspaces/demo");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&workspace).unwrap();
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
        .args(["manage", "edit", "demo", "--name", "renamed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated session demo -> renamed"));

    assert!(!sessions.join("demo.toml").exists());
    assert!(sessions.join("renamed.toml").exists());
    assert!(!home.join("workspaces/demo").exists());
    assert!(home.join("workspaces/renamed").exists());
}

#[test]
fn manage_remove_deletes_session_workspace_only() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let sessions = home.join("sessions");
    let workspace = home.join("workspaces/demo");
    let repo = temp.path().join("repo-a");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        sessions.join("demo.toml"),
        format!(
            r#"
name = "demo"
repos = [{{ name = "repo-a", path = "{}" }}]
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
            repo.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .args(["manage", "remove", "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed session demo"));

    assert!(!sessions.join("demo.toml").exists());
    assert!(!workspace.exists());
    assert!(repo.exists());
}

#[cfg(unix)]
#[test]
fn manage_reveal_uses_configured_file_manager() {
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("dynws-home");
    let sessions = home.join("sessions");
    let workspace = home.join("workspaces/demo");
    let fake_bin = temp.path().join("bin");
    let fake_open = fake_bin.join("fake-open");
    let log = temp.path().join("open.log");
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
    fs::write(
        home.join("config.toml"),
        format!(
            r#"
[file_manager]
default = "{}"
"#,
            fake_open.display()
        ),
    )
    .unwrap();
    write_executable(
        &fake_open,
        r#"#!/bin/sh
echo "$@" > "$DWS_TEST_OPEN_LOG"
"#,
    );

    Command::cargo_bin("dws")
        .unwrap()
        .env("DYNWS_HOME", &home)
        .env("DWS_TEST_OPEN_LOG", &log)
        .args(["manage", "reveal", "demo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("revealed demo with"));

    for _ in 0..20 {
        if log.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let logged = fs::read_to_string(log).unwrap();
    assert!(logged.contains(&workspace.display().to_string()));
}
