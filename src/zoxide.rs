use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::session::{SessionMetadata, SessionStore};

pub fn add_path_if_available(path: &Path) {
    let _ = add_path(path, false);
}

pub fn sync_sessions(store: &SessionStore, sessions: &[SessionMetadata]) -> Result<usize> {
    ensure_available()?;

    let mut synced = 0;
    for session in sessions {
        let workspace = store.workspace_path(&session.name);
        if workspace.is_dir() {
            add_path(&workspace, true)?;
            synced += 1;
        }
    }

    Ok(synced)
}

fn ensure_available() -> Result<()> {
    match Command::new("zoxide").arg("--version").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => bail!(
            "zoxide is installed but failed to run: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("zoxide is not installed or not on PATH")
        }
        Err(error) => Err(error).context("failed to run zoxide"),
    }
}

fn add_path(path: &Path, required: bool) -> Result<bool> {
    match Command::new("zoxide").arg("add").arg(path).output() {
        Ok(output) if output.status.success() => Ok(true),
        Ok(output) => {
            if required {
                bail!(
                    "zoxide add failed for {}: {}",
                    path.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("zoxide is not installed or not on PATH")
        }
        Err(error) => Err(error).context("failed to run zoxide"),
    }
}
