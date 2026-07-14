mod cli;
mod config;
mod discovery;
mod editor;
mod git;
mod session;
mod tui;

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::cli::{
    Cli, Commands, ConfigCommands, EditorCommands, FileManagerCommands, ManageCommands,
};
use crate::config::{Config, DynwsPaths};
use crate::editor::{
    Editor, FileManager, detect_editors, detect_file_managers, open_editor, open_file_manager,
    resolve_editor, resolve_file_manager,
};
use crate::session::{SessionMetadata, SessionPatch, SessionStore};

#[doc(hidden)]
pub fn run() -> Result<()> {
    let Cli {
        editor,
        no_open,
        command,
    } = Cli::parse();
    let cwd = std::env::current_dir().context("failed to read current directory")?;

    if command.is_some() && (editor.is_some() || no_open) {
        bail!("--editor and --no-open are only valid for the bare 'dws' interactive command");
    }

    if matches!(command, Some(Commands::Init)) {
        let paths = DynwsPaths::initialize_from(&cwd)?;
        println!(
            "initialized dws project for {}",
            paths.collection_root.display()
        );
        println!("storage: {}", paths.home.display());
        println!("marker: {}", paths.project_file.display());
        return Ok(());
    }

    // Resolving before dispatch is the single initialization guard for every
    // operational command, including the bare interactive flow.
    let paths = DynwsPaths::resolve_from(&cwd)?;

    match command {
        None => {
            if let Some(session) = tui::run_interactive(&paths, &paths.collection_root)? {
                let store = SessionStore::new(paths.clone());
                let workspace = store.workspace_path(&session.name);
                println!("session ready: {} ({})", session.name, workspace.display());
                if !no_open {
                    match open_session_with_default_editor(&paths, &session, editor.as_deref()) {
                        Ok(selected) => {
                            println!("opened {} with {}", session.name, selected.command_line())
                        }
                        Err(error) => {
                            println!("not opened: {error:#}");
                            println!("workspace: {}", workspace.display());
                        }
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
                            "dws manage requires a terminal; use 'dws manage edit', 'dws manage remove', or 'dws manage reveal' for scripts"
                        );
                    }
                    let changes =
                        tui::run_session_manager(&paths, &paths.collection_root, sessions)?;
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
        Some(Commands::Init) => unreachable!("init is handled before project resolution"),
    }

    Ok(())
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
            let updated = store.edit_session(
                &original_name,
                SessionPatch {
                    name,
                    description: if clear_description {
                        Some(None)
                    } else {
                        description.map(Some)
                    },
                },
            )?;
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

    Ok(())
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

    Ok(())
}
