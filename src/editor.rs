use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub id: String,
    pub label: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileManager {
    pub id: String,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
}

impl FileManager {
    pub fn command_line(&self) -> String {
        std::iter::once(self.command.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

struct KnownEditor {
    id: &'static str,
    label: &'static str,
    commands: &'static [&'static str],
}

struct KnownFileManager {
    id: &'static str,
    label: &'static str,
    command: &'static str,
    args: &'static [&'static str],
}

const KNOWN_EDITORS: &[KnownEditor] = &[
    KnownEditor {
        id: "code",
        label: "VS Code",
        commands: &["code", "code-insiders"],
    },
    KnownEditor {
        id: "cursor",
        label: "Cursor",
        commands: &["cursor"],
    },
    KnownEditor {
        id: "antigravity",
        label: "Antigravity",
        commands: &["antigravity"],
    },
    KnownEditor {
        id: "bob",
        label: "IBM Bob",
        commands: &["bobide"],
    },
];

const KNOWN_FILE_MANAGERS: &[KnownFileManager] = &[
    KnownFileManager {
        id: "finder",
        label: "Finder",
        command: "open",
        args: &[],
    },
    KnownFileManager {
        id: "xdg-open",
        label: "Linux desktop opener",
        command: "xdg-open",
        args: &[],
    },
    KnownFileManager {
        id: "gio",
        label: "GNOME file manager",
        command: "gio",
        args: &["open"],
    },
];

pub fn detect_editors() -> Vec<Editor> {
    let mut editors = Vec::new();
    let mut seen = HashSet::new();

    for known in KNOWN_EDITORS {
        for command in known.commands {
            if command_exists(command) && seen.insert((*command).to_string()) {
                editors.push(Editor {
                    id: known.id.to_string(),
                    label: known.label.to_string(),
                    command: (*command).to_string(),
                });
            }
        }
    }

    editors
}

pub fn detect_file_managers() -> Vec<FileManager> {
    let mut managers = Vec::new();
    let mut seen = HashSet::new();

    for known in KNOWN_FILE_MANAGERS {
        if command_exists(known.command) && seen.insert(command_line(known.command, known.args)) {
            managers.push(FileManager {
                id: known.id.to_string(),
                label: known.label.to_string(),
                command: known.command.to_string(),
                args: known.args.iter().map(|arg| (*arg).to_string()).collect(),
            });
        }
    }

    managers
}

pub fn resolve_editor(
    explicit: Option<&str>,
    default: Option<&str>,
    detected: &[Editor],
) -> Result<Editor> {
    if let Some(requested) = explicit
        .or(default)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(editor) = find_editor(requested, detected) {
            return Ok(editor.clone());
        }
        if command_exists(requested) {
            return Ok(Editor {
                id: requested.to_string(),
                label: requested.to_string(),
                command: requested.to_string(),
            });
        }
        bail!("editor command was not found: {requested}");
    }

    match detected {
        [single] => Ok(single.clone()),
        [] => {
            bail!("no supported editor commands detected; run `dws config editor set <command>`")
        }
        many => {
            let names = many
                .iter()
                .map(|editor| editor.command.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "multiple editors detected ({names}); choose one with --editor or `dws config editor set`"
            )
        }
    }
}

pub fn resolve_file_manager(
    explicit: Option<&str>,
    default: Option<&str>,
    detected: &[FileManager],
) -> Result<FileManager> {
    if let Some(requested) = explicit
        .or(default)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(manager) = find_file_manager(requested, detected) {
            return Ok(manager.clone());
        }
        if let Some(manager) = file_manager_from_command_line(requested) {
            return Ok(manager);
        }
        bail!("file manager command was not found: {requested}");
    }

    if cfg!(target_os = "macos") {
        if let Some(manager) = file_manager_from_command_line("open") {
            return Ok(manager);
        }
    }

    match detected {
        [single] => Ok(single.clone()),
        [] => {
            bail!(
                "no supported file manager command detected; run `dws config file-manager set <command>`"
            )
        }
        many => Ok(many[0].clone()),
    }
}

pub fn open_editor(editor: &Editor, path: &Path) -> Result<()> {
    Command::new(&editor.command).arg(path).spawn()?;
    Ok(())
}

pub fn open_file_manager(file_manager: &FileManager, path: &Path) -> Result<()> {
    Command::new(&file_manager.command)
        .args(&file_manager.args)
        .arg(path)
        .spawn()?;
    Ok(())
}

fn find_editor<'a>(requested: &str, detected: &'a [Editor]) -> Option<&'a Editor> {
    detected.iter().find(|editor| {
        editor.id.eq_ignore_ascii_case(requested)
            || editor.command.eq_ignore_ascii_case(requested)
            || editor.label.eq_ignore_ascii_case(requested)
    })
}

fn find_file_manager<'a>(requested: &str, detected: &'a [FileManager]) -> Option<&'a FileManager> {
    detected.iter().find(|manager| {
        manager.id.eq_ignore_ascii_case(requested)
            || manager.command.eq_ignore_ascii_case(requested)
            || manager.label.eq_ignore_ascii_case(requested)
            || manager.command_line().eq_ignore_ascii_case(requested)
    })
}

fn file_manager_from_command_line(command_line: &str) -> Option<FileManager> {
    let parts = command_line
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let (command, args) = parts.split_first()?;
    if !command_exists(command) {
        return None;
    }

    Some(FileManager {
        id: command_line.to_string(),
        label: command_line.to_string(),
        command: (*command).to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
    })
}

fn command_line(command: &str, args: &[&str]) -> String {
    std::iter::once(command)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return is_executable(path);
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|directory| is_executable(&directory.join(command)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editors() -> Vec<Editor> {
        vec![
            Editor {
                id: "code".to_string(),
                label: "VS Code".to_string(),
                command: "code".to_string(),
            },
            Editor {
                id: "cursor".to_string(),
                label: "Cursor".to_string(),
                command: "cursor".to_string(),
            },
        ]
    }

    fn file_managers() -> Vec<FileManager> {
        vec![
            FileManager {
                id: "finder".to_string(),
                label: "Finder".to_string(),
                command: "open".to_string(),
                args: Vec::new(),
            },
            FileManager {
                id: "gio".to_string(),
                label: "GNOME file manager".to_string(),
                command: "gio".to_string(),
                args: vec!["open".to_string()],
            },
        ]
    }

    #[test]
    fn explicit_editor_wins_over_default() {
        let selected = resolve_editor(Some("cursor"), Some("code"), &editors()).unwrap();

        assert_eq!(selected.command, "cursor");
    }

    #[test]
    fn default_editor_is_used_when_no_override_exists() {
        let selected = resolve_editor(None, Some("code"), &editors()).unwrap();

        assert_eq!(selected.command, "code");
    }

    #[test]
    fn errors_when_multiple_editors_have_no_default() {
        let error = resolve_editor(None, None, &editors()).unwrap_err();

        assert!(error.to_string().contains("multiple editors"));
    }

    #[test]
    fn resolves_known_file_manager_by_id() {
        let selected = resolve_file_manager(Some("gio"), None, &file_managers()).unwrap();

        assert_eq!(selected.command_line(), "gio open");
    }

    #[test]
    fn uses_first_detected_file_manager_when_unset() {
        let selected = resolve_file_manager(None, None, &file_managers()).unwrap();

        assert_eq!(selected.command, "open");
    }
}
