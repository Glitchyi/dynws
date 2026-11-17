use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::editor::{self, LaunchCommand};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Opener {
    #[default]
    Editor,
    T3,
    Finder,
}

impl Opener {
    pub const ALL: [Self; 3] = [Self::Editor, Self::T3, Self::Finder];

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "editor" | "cursor" => Ok(Self::Editor),
            "t3" => Ok(Self::T3),
            "finder" => Ok(Self::Finder),
            _ => bail!("expected editor, t3 or finder, got {value:?}"),
        }
    }

    pub fn label(self, config: &Config) -> &str {
        match self {
            Self::Editor => config.editor.default.as_deref().unwrap_or("Cursor"),
            Self::T3 => "T3 Code",
            Self::Finder => "Finder",
        }
    }
}

/// An argument-vector plan is both the confirmation text and the executable
/// operation. Workspace paths never pass through a shell.
pub struct OpenPlan {
    command: LaunchCommand,
    detached: bool,
}

impl OpenPlan {
    pub fn new(
        opener: Opener,
        config: &Config,
        workspace: &Path,
        explicit_editor: Option<&str>,
    ) -> Result<Self> {
        let path = workspace.to_string_lossy().into_owned();
        let (mut command, detached) = match opener {
            Opener::Editor => (
                editor::resolve_editor(
                    explicit_editor,
                    config
                        .editor
                        .default
                        .as_deref()
                        .or(Some("cursor --new-window")),
                )?,
                config.open.background,
            ),
            Opener::Finder => (
                editor::resolve_file_manager(
                    None,
                    config.file_manager.default.as_deref(),
                    &editor::detect_file_managers(),
                )?,
                false,
            ),
            Opener::T3 => {
                let binary = t3_binary()?;
                let command = binary.to_string_lossy().into_owned();
                (
                    LaunchCommand {
                        id: "t3".into(),
                        label: "T3 Code".into(),
                        command: command.clone(),
                        args: vec!["app".into()],
                    },
                    false,
                )
            }
        };
        command.args.push(path);
        Ok(Self { command, detached })
    }

    pub fn description(&self) -> String {
        self.command.command_line()
    }

    pub fn run(&self) -> Result<String> {
        let mut transcript = String::new();
        let command = &self.command;
        transcript.push_str(&format!("$ {}\n", command.command_line()));
        if self.detached {
            let (path, args) = command
                .args
                .split_last()
                .context("missing workspace argument")?;
            let mut launch = command.clone();
            launch.args = args.to_vec();
            editor::launch_detached(&launch, Path::new(path))?;
        } else {
            let output = Command::new(&command.command)
                .args(&command.args)
                .stdin(std::process::Stdio::null())
                .output()
                .with_context(|| format!("failed to start {}", command.command_line()))?;
            transcript.push_str(&String::from_utf8_lossy(&output.stdout));
            transcript.push_str(&String::from_utf8_lossy(&output.stderr));
            if !output.status.success() {
                bail!(
                    "{} exited with {}\n{transcript}",
                    command.label,
                    output.status
                );
            }
        }
        Ok(transcript)
    }
}

fn t3_binary() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unset; cannot locate ~/.t3/bin/t3")?;
    let binary = PathBuf::from(home).join(".t3/bin/t3");
    if !binary.is_file() {
        bail!("T3 Code CLI is missing: {}", binary.display());
    }
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_keeps_workspace_as_one_literal_argument_and_captures_failure() {
        let config = Config::default();
        let workspace = Path::new("/tmp/work space;$(ignored)`ignored`");
        let plan = OpenPlan::new(
            Opener::Editor,
            &config,
            workspace,
            Some("sh -c 'printf %s \"$0\"; printf failed >&2; exit 9'"),
        )
        .unwrap();
        assert_eq!(
            plan.command.args.last().unwrap(),
            workspace.to_str().unwrap()
        );
        let error = plan.run().unwrap_err().to_string();
        assert!(error.contains("exit status: 9"));
        assert!(error.contains("/tmp/work space;$(ignored)`ignored`failed"));
        assert_eq!(
            editor::parse_command_line(&plan.description())
                .unwrap()
                .last()
                .unwrap(),
            workspace.to_str().unwrap()
        );
    }
}
