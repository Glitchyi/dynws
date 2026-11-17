use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub id: String,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
}

impl LaunchCommand {
    pub fn command_line(&self) -> String {
        command_line(
            &self.command,
            &self.args.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }
}

struct KnownFileManager {
    id: &'static str,
    label: &'static str,
    command: &'static str,
    args: &'static [&'static str],
}

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
    KnownFileManager {
        id: "nautilus",
        label: "Nautilus",
        command: "nautilus",
        args: &[],
    },
    KnownFileManager {
        id: "dolphin",
        label: "Dolphin",
        command: "dolphin",
        args: &[],
    },
    KnownFileManager {
        id: "thunar",
        label: "Thunar",
        command: "thunar",
        args: &[],
    },
];

pub fn detect_file_managers() -> Vec<LaunchCommand> {
    let mut managers = Vec::new();
    let mut seen = HashSet::new();

    for known in KNOWN_FILE_MANAGERS {
        if command_exists(known.command) && seen.insert(command_line(known.command, known.args)) {
            managers.push(LaunchCommand {
                id: known.id.to_string(),
                label: known.label.to_string(),
                command: known.command.to_string(),
                args: known.args.iter().map(|arg| (*arg).to_string()).collect(),
            });
        }
    }

    managers
}

pub fn resolve_editor(explicit: Option<&str>, default: Option<&str>) -> Result<LaunchCommand> {
    let requested = explicit
        .or(default)
        .context("no editor configured; use --editor or 'dws config set editor <command>'")?;
    from_command_line(requested).context("failed to resolve editor command")
}

pub fn resolve_file_manager(
    explicit: Option<&str>,
    default: Option<&str>,
    detected: &[LaunchCommand],
) -> Result<LaunchCommand> {
    if let Some(requested) = explicit
        .or(default)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(manager) = find_file_manager(requested, detected) {
            return Ok(manager.clone());
        }
        return from_command_line(requested)
            .with_context(|| format!("failed to resolve file manager command '{requested}'"));
    }

    for command_line in default_file_manager_command_lines() {
        if let Ok(manager) = from_command_line(command_line) {
            return Ok(manager);
        }
    }

    match detected {
        [single] => Ok(single.clone()),
        [] => {
            bail!(
                "no supported file manager command detected; run `dws config set file-manager <command>`"
            )
        }
        many => Ok(many[0].clone()),
    }
}

pub fn launch(command: &LaunchCommand, path: &Path, wait: bool) -> Result<()> {
    let mut process = Command::new(&command.command);
    process.args(&command.args).arg(path);
    if wait {
        let status = process.status().context("failed to launch editor")?;
        if !status.success() {
            bail!("editor exited with {status}");
        }
    } else {
        process.spawn().context("failed to launch file manager")?;
    }
    Ok(())
}

fn find_file_manager<'a>(
    requested: &str,
    detected: &'a [LaunchCommand],
) -> Option<&'a LaunchCommand> {
    detected.iter().find(|manager| {
        manager.id.eq_ignore_ascii_case(requested)
            || manager.command.eq_ignore_ascii_case(requested)
            || manager.label.eq_ignore_ascii_case(requested)
            || manager.command_line().eq_ignore_ascii_case(requested)
    })
}

fn from_command_line(command_line: &str) -> Result<LaunchCommand> {
    let parts = parse_command_line(command_line)?;
    let (command, args) = parts
        .split_first()
        .expect("parse_command_line returns at least one token");
    if !command_exists(command) {
        bail!("launch command was not found: {command}");
    }

    Ok(LaunchCommand {
        id: command_line.to_string(),
        label: command_line.to_string(),
        command: (*command).to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
    })
}

pub fn parse_command_line(input: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut in_token = false;

    for character in input.trim().chars() {
        if escaped {
            current.push(character);
            escaped = false;
            in_token = true;
            continue;
        }

        if quote != Some('\'') && character == '\\' {
            escaped = true;
            in_token = true;
            continue;
        }

        if let Some(quote_char) = quote {
            if character == quote_char {
                quote = None;
            } else {
                current.push(character);
            }
            in_token = true;
            continue;
        }

        match character {
            '"' | '\'' => {
                quote = Some(character);
                in_token = true;
            }
            value if value.is_whitespace() => {
                if in_token {
                    parts.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            value => {
                current.push(value);
                in_token = true;
            }
        }
    }

    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        bail!("unterminated quote in command line");
    }
    if in_token {
        parts.push(current);
    }
    if parts.is_empty() {
        bail!("command line cannot be empty");
    }

    Ok(parts)
}

fn command_line(command: &str, args: &[&str]) -> String {
    std::iter::once(command)
        .chain(args.iter().copied())
        .map(quote_command_part)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_command_part(part: &str) -> String {
    if !part.is_empty()
        && !part.chars().any(|character| {
            character.is_whitespace() || character == '\'' || character == '"' || character == '\\'
        })
    {
        return part.to_string();
    }

    let mut quoted = String::from("\"");
    for character in part.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

fn default_file_manager_command_lines() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &["open"]
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        &["xdg-open", "gio open", "nautilus", "dolphin", "thunar"]
    }

    #[cfg(not(unix))]
    {
        &[]
    }
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

    fn file_managers() -> Vec<LaunchCommand> {
        vec![
            LaunchCommand {
                id: "finder".to_string(),
                label: "Finder".to_string(),
                command: "open".to_string(),
                args: Vec::new(),
            },
            LaunchCommand {
                id: "gio".to_string(),
                label: "GNOME file manager".to_string(),
                command: "gio".to_string(),
                args: vec!["open".to_string()],
            },
        ]
    }

    #[test]
    fn explicit_editor_wins_and_no_catalog_is_required() {
        assert_eq!(
            resolve_editor(Some("sh -e"), Some("missing-editor"))
                .unwrap()
                .command_line(),
            "sh -e"
        );
        assert_eq!(resolve_editor(None, Some("sh")).unwrap().command, "sh");
        assert!(
            resolve_editor(None, None)
                .unwrap_err()
                .to_string()
                .contains("no editor configured")
        );
    }

    #[test]
    fn parses_command_lines_with_quotes() {
        assert_eq!(
            parse_command_line(r#"gio open "folder name" 'other value'"#).unwrap(),
            vec!["gio", "open", "folder name", "other value"]
        );
        assert!(parse_command_line(r#"gio "open"#).is_err());
        assert!(parse_command_line("   ").is_err());
    }

    #[test]
    fn formats_command_lines_with_spaces_for_round_trip() {
        let formatted = command_line("/tmp/my opener", &["--flag", "two words"]);

        assert_eq!(formatted, r#""/tmp/my opener" --flag "two words""#);
        assert_eq!(
            parse_command_line(&formatted).unwrap(),
            vec!["/tmp/my opener", "--flag", "two words"]
        );
    }

    #[test]
    fn resolves_custom_editor_command_lines() {
        let selected = resolve_editor(Some("sh -c"), None).unwrap();

        assert_eq!(selected.command, "sh");
        assert_eq!(selected.args, vec!["-c"]);
        assert_eq!(selected.command_line(), "sh -c");
    }

    #[test]
    fn resolves_known_file_manager_by_id() {
        let selected = resolve_file_manager(Some("gio"), None, &file_managers()).unwrap();

        assert_eq!(selected.command_line(), "gio open");
    }

    #[test]
    fn resolves_custom_file_manager_command_lines() {
        let selected = resolve_file_manager(Some("sh -c"), None, &[]).unwrap();

        assert_eq!(selected.command, "sh");
        assert_eq!(selected.args, vec!["-c"]);
        assert_eq!(selected.command_line(), "sh -c");
    }

    #[test]
    fn uses_platform_file_manager_when_unset() {
        let selected = resolve_file_manager(None, None, &file_managers()).unwrap();

        let expected = default_file_manager_command_lines()
            .iter()
            .find(|command| from_command_line(command).is_ok())
            .map(|command| (*command).to_owned())
            .unwrap_or_else(|| file_managers()[0].command_line());
        assert_eq!(selected.command_line(), expected);
    }
}
