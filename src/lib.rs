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

use crate::cli::{
    Cli, Commands, ConfigCommands, EditorCommands, FileManagerCommands, ManageCommands, Shell,
    ZoxideCommands,
};
use crate::config::{Config, DynwsPaths, SetupConfig, write_setup_config};
use crate::editor::{
    Editor, FileManager, detect_editors, detect_file_managers, open_editor, open_file_manager,
    resolve_editor, resolve_file_manager,
};
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
                        Ok(editor) => {
                            println!("opened {} with {}", session.name, editor.command_line())
                        }
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
                    Ok(selected) => {
                        println!("opened {} with {}", session.name, selected.command_line())
                    }
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
            println!("opened {} with {}", metadata.name, selected.command_line());
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
        Some(Commands::Manage { command }) => {
            let store = SessionStore::new(paths.clone());
            match command {
                Some(command) => handle_manage_command(&paths, &store, command)?,
                None => {
                    let sessions = store.load_sessions()?;
                    if sessions.is_empty() {
                        println!("no sessions found");
                        return Ok(());
                    }
                    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                        bail!(
                            "dws manage requires a terminal; use 'dws manage edit' or 'dws manage remove' for scripts"
                        );
                    }
                    let changes = tui::run_session_manager(&paths, &cwd, sessions)?;
                    for change in changes {
                        println!("{change}");
                    }
                }
            }
        }
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Editor { command } => handle_editor_config(&paths, command)?,
            ConfigCommands::FileManager { command } => handle_file_manager_config(&paths, command)?,
        },
        Some(Commands::Setup {
            editor,
            file_manager,
            yes,
        }) => {
            handle_setup_command(&paths, editor, file_manager, yes)?;
        }
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

fn reveal_session_with_default_file_manager(
    paths: &DynwsPaths,
    store: &SessionStore,
    session: &SessionMetadata,
) -> Result<FileManager> {
    let workspace = store.workspace_path(&session.name);
    let config = Config::load(paths)?;
    let detected = detect_file_managers();
    let selected = resolve_file_manager(None, config.file_manager.default.as_deref(), &detected)?;
    open_file_manager(&selected, &workspace)?;
    Ok(selected)
}

fn handle_manage_command(
    paths: &DynwsPaths,
    store: &SessionStore,
    command: ManageCommands,
) -> Result<()> {
    match command {
        ManageCommands::Edit {
            session,
            name,
            description,
            clear_description,
        } => {
            if name.is_none() && description.is_none() && !clear_description {
                bail!("nothing to edit; pass --name, --description, or --clear-description");
            }
            if description.is_some() && clear_description {
                bail!("use either --description or --clear-description, not both");
            }

            let original_name = store.load_session(&session)?.name;
            let mut current_name = original_name.clone();
            let mut updated = None;

            if let Some(new_name) = name {
                let metadata = store.rename_session(&current_name, &new_name)?;
                current_name = metadata.name.clone();
                updated = Some(metadata);
            }

            if description.is_some() || clear_description {
                let metadata = store.set_session_description(
                    &current_name,
                    if clear_description { None } else { description },
                )?;
                updated = Some(metadata);
            }

            let updated = updated.context("no session update was applied")?;
            if original_name == updated.name {
                println!("updated session {}", updated.name);
            } else {
                println!("updated session {} -> {}", original_name, updated.name);
            }
        }
        ManageCommands::Remove { session } => {
            let removed = store.remove_session(&session)?;
            println!("removed session {}", removed.name);
        }
        ManageCommands::Reveal { session } => {
            let metadata = store.load_session(&session)?;
            let selected = reveal_session_with_default_file_manager(paths, store, &metadata)?;
            println!(
                "revealed {} with {}",
                metadata.name,
                selected.command_line()
            );
        }
    }

    Ok(())
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
                let command_line = editor.command_line();
                let marker = if config.editor.default.as_deref() == Some(command_line.as_str()) {
                    "default"
                } else {
                    "detected"
                };
                println!("{} ({}) - {}", command_line, editor.label, marker);
            }
        }
        EditorCommands::Set { editor } => {
            let detected = detect_editors();
            let selected = resolve_editor(Some(&editor), None, &detected)
                .with_context(|| format!("failed to resolve editor '{editor}'"))?;
            config.editor.default = Some(selected.command_line());
            config.save(paths)?;
            println!("default editor set to {}", selected.command_line());
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

fn handle_setup_command(
    paths: &DynwsPaths,
    editor: Option<String>,
    file_manager: Option<String>,
    yes: bool,
) -> Result<()> {
    let current = Config::load(paths)?;
    let initial = SetupConfig {
        editor: editor.or(current.editor.default),
        file_manager: file_manager.or(current.file_manager.default),
    };

    let setup = if yes {
        Some(normalize_setup_config(initial)?)
    } else if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        tui::run_setup(paths, initial)?
            .map(normalize_setup_config)
            .transpose()?
    } else {
        bail!("dws setup requires a terminal; pass --yes for non-interactive setup")
    };

    let Some(setup) = setup else {
        println!("setup cancelled");
        return Ok(());
    };

    write_setup_config(paths, &setup)?;
    println!("initialized dws project at {}", paths.home.display());
    println!("config: {}", paths.config_file.display());
    if let Some(editor) = &setup.editor {
        println!("default editor: {editor}");
    }
    if let Some(file_manager) = &setup.file_manager {
        println!("default file manager: {file_manager}");
    }
    Ok(())
}

fn normalize_setup_config(setup: SetupConfig) -> Result<SetupConfig> {
    let editor = setup
        .editor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let detected = detect_editors();
            resolve_editor(Some(value), None, &detected).map(|editor| editor.command_line())
        })
        .transpose()?;
    let file_manager = setup
        .file_manager
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let detected = detect_file_managers();
            resolve_file_manager(Some(value), None, &detected)
                .map(|file_manager| file_manager.command_line())
        })
        .transpose()?;

    Ok(SetupConfig {
        editor,
        file_manager,
    })
}

fn handle_file_manager_config(paths: &DynwsPaths, command: FileManagerCommands) -> Result<()> {
    let mut config = Config::load(paths)?;

    match command {
        FileManagerCommands::List => {
            let detected = detect_file_managers();
            if detected.is_empty() {
                println!("no supported file manager commands detected");
                return Ok(());
            }

            for file_manager in detected {
                let command_line = file_manager.command_line();
                let marker =
                    if config.file_manager.default.as_deref() == Some(command_line.as_str()) {
                        "default"
                    } else {
                        "detected"
                    };
                println!("{} ({}) - {}", command_line, file_manager.label, marker);
            }
        }
        FileManagerCommands::Set { file_manager } => {
            let detected = detect_file_managers();
            let selected = resolve_file_manager(Some(&file_manager), None, &detected)
                .with_context(|| format!("failed to resolve file manager '{file_manager}'"))?;
            config.file_manager.default = Some(selected.command_line());
            config.save(paths)?;
            println!("default file manager set to {}", selected.command_line());
        }
        FileManagerCommands::Clear => {
            if config.file_manager.default.is_none() {
                println!("default file manager already unset");
                return Ok(());
            }
            config.file_manager.default = None;
            config.save(paths)?;
            println!("default file manager cleared");
        }
    }

    if let Some(default) = &config.file_manager.default {
        if default.trim().is_empty() {
            bail!("default file manager cannot be empty");
        }
    }

    Ok(())
}
