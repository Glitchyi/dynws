use std::ffi::OsStr;
use std::fs;

use clap_complete::CompletionCandidate;

use crate::config::DynwsPaths;
use crate::session::{SessionMetadata, normalize_name};

pub fn sessions(prefix: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(paths) = DynwsPaths::resolve_from(&cwd) else {
        return Vec::new();
    };
    names(&paths, prefix)
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

// Completion suggests names only. Operational commands still perform full
// metadata, workspace and Git ownership validation before using a selection.
fn names(paths: &DynwsPaths, prefix: &OsStr) -> Vec<String> {
    let Ok(entries) = fs::read_dir(&paths.sessions_dir) else {
        return Vec::new();
    };
    let Some(prefix) = prefix.to_str() else {
        return Vec::new();
    };
    let mut names = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let path = entry.path();
            if path.extension()? != "toml" {
                return None;
            }
            let name = path.file_stem()?.to_str()?;
            if !name.starts_with(prefix) || normalize_name(name).ok()? != name {
                return None;
            }
            let data = fs::read_to_string(&path).ok()?;
            let metadata: SessionMetadata = toml::from_str(&data).ok()?;
            (metadata.name == name).then(|| name.to_owned())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(paths: &DynwsPaths, name: &str) {
        fs::write(
            paths.sessions_dir.join(format!("{name}.toml")),
            format!("name = {name:?}\nrepos = []\ncreated_at = 'now'\nupdated_at = 'now'\n"),
        )
        .unwrap();
    }

    #[test]
    fn names_are_fresh_sorted_scoped_and_skip_bad_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DynwsPaths::from_home(temp.path().join(".dynws"));
        session(&paths, "api-review");
        session(&paths, "api-fix");
        fs::write(paths.sessions_dir.join("bad.toml"), "invalid").unwrap();
        fs::copy(
            paths.sessions_dir.join("api-fix.toml"),
            paths.sessions_dir.join("mismatch.toml"),
        )
        .unwrap();
        assert_eq!(names(&paths, OsStr::new("api")), ["api-fix", "api-review"]);
        fs::rename(
            paths.sessions_dir.join("api-fix.toml"),
            paths.sessions_dir.join("old.tmp"),
        )
        .unwrap();
        session(&paths, "new-session");
        assert_eq!(names(&paths, OsStr::new("")), ["api-review", "new-session"]);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                paths.sessions_dir.join("new-session.toml"),
                paths.sessions_dir.join("symlink.toml"),
            )
            .unwrap();
            assert_eq!(names(&paths, OsStr::new("s")), Vec::<String>::new());
        }
    }
}
