pub mod cli;
pub mod config;
pub mod discovery;
pub mod editor;
pub mod git;
pub mod session;
pub mod tui;
pub mod zoxide;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::IsTerminal;
use std::path::Path;

use crate::cli::{Cli, Commands, ConfigCommands, EditorCommands, Shell, ZoxideCommands};
use crate::config::{Config, DynwsPaths};
use crate::editor::{Editor, detect_editors, open_editor, resolve_editor};
use crate::session::{SessionMetadata, SessionStore};

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = DynwsPaths::resolve_from(&cwd)?;

    match cli.command {
        None => {
            if let Some(session) = tui::run_interactive(&paths, &cwd)? {
                let store = SessionStore::new(paths.clone());
                let workspace = store.workspace_path(&session.name);
                zoxide::add_path_if_available(&workspace);
                println!("session ready: {} ({})", session.name, workspace.display());
                if !cli.no_open {
                    match open_session_with_default_editor(&paths, &session, cli.editor.as_deref())
                    {
                        Ok(editor) => println!("opened {} with {}", session.name, editor.command),
                        Err(error) => {
                            println!("not opened: {error:#}");
                            println!("cd target: {}", workspace.display());
                        }
                    }
                }
            }
        }
        Some(Commands::List { plain, editor }) => {
            let store = SessionStore::new(paths.clone());
            let sessions = store.load_sessions()?;
            if sessions.is_empty() {
                println!("no sessions found");
                return Ok(());
            }

            if plain || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                print_sessions(&sessions);
            } else if let Some(session) = tui::run_session_picker(&paths, sessions)? {
                let workspace = store.workspace_path(&session.name);
                match open_session_with_default_editor(&paths, &session, editor.as_deref()) {
                    Ok(selected) => println!("opened {} with {}", session.name, selected.command),
                    Err(error) => {
                        println!("not opened: {error:#}");
                        println!("cd target: {}", workspace.display());
                    }
                }
            }
        }
        Some(Commands::Open { session, editor }) => {
            let store = SessionStore::new(paths.clone());
            let metadata = store.load_session(&session)?;
            let selected = open_session_with_default_editor(&paths, &metadata, editor.as_deref())?;
            println!("opened {} with {}", metadata.name, selected.command);
        }
        Some(Commands::Path { session }) => {
            let store = SessionStore::new(paths);
            let metadata = store.load_session(&session)?;
            println!("{}", store.workspace_path(&metadata.name).display());
        }
        Some(Commands::Init { shell }) => {
            print_shell_init(shell);
        }
        Some(Commands::Zoxide { command }) => match command {
            ZoxideCommands::Sync => {
                let store = SessionStore::new(paths);
                let sessions = store.load_sessions()?;
                let synced = zoxide::sync_sessions(&store, &sessions)?;
                println!("synced {synced} workspace(s) to zoxide");
            }
        },
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Editor { command } => handle_editor_config(&paths, command)?,
        },
    }

    Ok(())
}

fn print_shell_init(shell: Shell) {
    match shell {
        Shell::Bash | Shell::Zsh => print!("{POSIX_SHELL_INIT}"),
        Shell::Fish => print!("{FISH_SHELL_INIT}"),
    }
}

const POSIX_SHELL_INIT: &str = r#"# dws shell integration
dws-cd() {
  if [ "$#" -lt 1 ]; then
    printf '%s\n' 'usage: dws-cd <session>' >&2
    return 2
  fi

  local target
  target="$(dws path "$1")" || return

  if command -v zoxide >/dev/null 2>&1; then
    zoxide add "$target" >/dev/null 2>&1 || true
  fi

  cd "$target"
}

dws-z() {
  if ! command -v zoxide >/dev/null 2>&1; then
    printf '%s\n' 'zoxide is not installed or not on PATH' >&2
    return 127
  fi

  dws zoxide sync >/dev/null 2>&1 || true

  local target
  target="$(zoxide query "$@")" || return
  cd "$target"
}
"#;

const FISH_SHELL_INIT: &str = r#"# dws shell integration
function dws-cd
  if test (count $argv) -lt 1
    echo 'usage: dws-cd <session>' >&2
    return 2
  end

  set -l target (dws path $argv[1]); or return

  if command -q zoxide
    zoxide add $target >/dev/null 2>&1; or true
  end

  cd $target
end

function dws-z
  if not command -q zoxide
    echo 'zoxide is not installed or not on PATH' >&2
    return 127
  end

  dws zoxide sync >/dev/null 2>&1; or true

  set -l target (zoxide query $argv); or return
  cd $target
end
"#;

fn print_sessions(sessions: &[SessionMetadata]) {
    for session in sessions {
        println!("{}", session.name);
        if let Some(description) = &session.description {
            println!("  {description}");
        }
        for repo in &session.repos {
            println!("  - {} -> {}", repo.name, repo.path);
        }
    }
}

fn open_session_with_default_editor(
    paths: &DynwsPaths,
    session: &SessionMetadata,
    explicit_editor: Option<&str>,
) -> Result<Editor> {
    let store = SessionStore::new(paths.clone());
    let workspace = store.workspace_path(&session.name);
    open_workspace_with_default_editor(paths, &workspace, explicit_editor)
}

fn open_workspace_with_default_editor(
    paths: &DynwsPaths,
    workspace: &Path,
    explicit_editor: Option<&str>,
) -> Result<Editor> {
    let config = Config::load(paths)?;
    let detected = detect_editors();
    let selected = resolve_editor(explicit_editor, config.editor.default.as_deref(), &detected)?;
    zoxide::add_path_if_available(workspace);
    open_editor(&selected, workspace)?;
    Ok(selected)
}

fn handle_editor_config(paths: &DynwsPaths, command: EditorCommands) -> Result<()> {
    let mut config = Config::load(paths)?;

    match command {
        EditorCommands::List => {
            let detected = detect_editors();
            if detected.is_empty() {
                println!("no supported editor commands detected");
                return Ok(());
            }

            for editor in detected {
                let marker = if config.editor.default.as_deref() == Some(editor.command.as_str()) {
                    "default"
                } else {
                    "detected"
                };
                println!("{} ({}) - {}", editor.command, editor.label, marker);
            }
        }
        EditorCommands::Set { editor } => {
            let detected = detect_editors();
            let selected = resolve_editor(Some(&editor), None, &detected)
                .with_context(|| format!("failed to resolve editor '{editor}'"))?;
            config.editor.default = Some(selected.command.clone());
            config.save(paths)?;
            println!("default editor set to {}", selected.command);
        }
        EditorCommands::Clear => {
            if config.editor.default.is_none() {
                println!("default editor already unset");
                return Ok(());
            }
            config.editor.default = None;
            config.save(paths)?;
            println!("default editor cleared");
        }
    }

    if let Some(default) = &config.editor.default {
        if default.trim().is_empty() {
            bail!("default editor cannot be empty");
        }
    }

    Ok(())
}
