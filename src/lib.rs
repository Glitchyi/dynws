mod cli;
mod completion;
mod config;
mod creation;
mod discovery;
mod editor;
mod git;
mod manifest;
mod session;
mod tui;

use crate::cli::{Cli, Commands, ConfigCommands, ConfigKey};
use crate::config::{Config, DynwsPaths};
use crate::editor::{detect_file_managers, launch, resolve_editor, resolve_file_manager};
use crate::session::{SessionPatch, SessionStore};
use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
use std::io::IsTerminal;

#[doc(hidden)]
pub fn run() -> Result<()> {
    clap_complete::CompleteEnv::with_factory(Cli::command)
        .var("DWS_COMPLETE")
        .complete();
    let Cli { command } = Cli::parse();
    let cwd = std::env::current_dir().context("failed to read current directory")?;
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
    if let Some(Commands::Create {
        file,
        input_format,
        project,
        fetch,
        reuse,
        json,
        toml,
    }) = &command
    {
        let paths = match project {
            Some(project) => DynwsPaths::load_initialized(&cwd.join(project).join(".dynws"))?,
            None => DynwsPaths::resolve_from(&cwd)?,
        };
        let result = creation::create(&paths, file, *input_format, &cwd, *fetch, *reuse)?;
        let output = if *json {
            serde_json::to_string(&result)?
        } else if *toml {
            toml::to_string_pretty(&result)?
        } else {
            result.path.display().to_string()
        };
        for warning in &result.warnings {
            eprintln!("warning: {warning}");
        }
        println!("{output}");
        return Ok(());
    }

    let paths = DynwsPaths::resolve_from(&cwd)?;
    let store = SessionStore::new(paths.clone());
    match command {
        None => {
            require_terminal()?;
            let outcome =
                tui::run_session_manager(&paths, &paths.collection_root, store.load_sessions()?)?;
            for change in outcome.changes {
                eprintln!("{change}");
            }
            if let Some(session) = outcome.open_session {
                open_session(&paths, &session, None)?;
            }
        }
        Some(Commands::New { editor, no_open }) => {
            require_terminal()?;
            if let tui::RepoFlowOutcome::Completed(session) =
                tui::run_interactive(&paths, &paths.collection_root)?
            {
                println!("{}", store.workspace_path(&session.name).display());
                if !no_open
                    && let Err(error) = open_session(&paths, &session.name, editor.as_deref())
                {
                    eprintln!(
                        "warning: session {} is ready, but was not opened: {error:#}",
                        session.name
                    );
                }
            }
        }
        Some(Commands::Open { session, editor }) => {
            open_session(&paths, &session, editor.as_deref())?
        }
        Some(Commands::Edit {
            session,
            name,
            description,
            clear_description,
        }) => {
            if name.is_none() && description.is_none() && !clear_description {
                bail!("nothing to edit; pass --name, --description, or --clear-description");
            }
            let updated = store.edit_session(
                &session,
                SessionPatch {
                    name,
                    description: if clear_description {
                        Some(None)
                    } else {
                        description.map(Some)
                    },
                },
            )?;
            if session == updated.name {
                println!("updated session {}", updated.name);
            } else {
                println!("updated session {} -> {}", session, updated.name);
            }
        }
        Some(Commands::Remove { session }) => {
            let outcome = store.remove_sessions(&[session])?;
            for removed in outcome.sessions {
                println!("removed session {}", removed.name);
            }
            for warning in outcome.warnings {
                eprintln!("warning: {warning}");
            }
        }
        Some(Commands::Reveal { session }) => {
            let metadata = store.load_session(&session)?;
            let config = Config::load(&paths)?;
            let selected = resolve_file_manager(
                None,
                config.file_manager.default.as_deref(),
                &detect_file_managers(),
            )?;
            launch(&selected, &store.workspace_path(&metadata.name), false)?;
            println!(
                "revealed {} with {}",
                metadata.name,
                selected.command_line()
            );
        }
        Some(Commands::Config { command }) => configure(&paths, command)?,
        Some(Commands::Init | Commands::Create { .. }) => unreachable!("handled before dispatch"),
    }
    Ok(())
}

fn require_terminal() -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "dws and dws new require a terminal; use 'dws create' for manifest creation or open/edit/remove/reveal for quick actions"
        );
    }
    Ok(())
}

fn open_session(paths: &DynwsPaths, name: &str, explicit: Option<&str>) -> Result<()> {
    let store = SessionStore::new(paths.clone());
    let metadata = store.load_session(name)?;
    let config = Config::load(paths)?;
    let selected = resolve_editor(explicit, config.editor.default.as_deref())?;
    launch(&selected, &store.workspace_path(&metadata.name), true)
}

fn configure(paths: &DynwsPaths, command: ConfigCommands) -> Result<()> {
    let mut config = Config::load(paths)?;
    match command {
        ConfigCommands::List { .. } => {
            let managers = detect_file_managers();
            if managers.is_empty() {
                println!("no supported file manager commands detected");
            }
            for manager in managers {
                let command = manager.command_line();
                let marker = if config.file_manager.default.as_deref() == Some(command.as_str()) {
                    "default"
                } else {
                    "detected"
                };
                println!("{} ({}) - {marker}", command, manager.label);
            }
        }
        ConfigCommands::Set { key, command } => {
            let selected = match key {
                ConfigKey::Editor => resolve_editor(Some(&command), None)?,
                ConfigKey::FileManager => {
                    resolve_file_manager(Some(&command), None, &detect_file_managers())?
                }
            };
            *setting(&mut config, key) = Some(selected.command_line());
            config.save(paths)?;
            println!("default {} set to {}", key.label(), selected.command_line());
        }
        ConfigCommands::Clear { key } => {
            if setting(&mut config, key).take().is_none() {
                println!("default {} already unset", key.label());
            } else {
                config.save(paths)?;
                println!("default {} cleared", key.label());
            }
        }
    }
    Ok(())
}

fn setting(config: &mut Config, key: ConfigKey) -> &mut Option<String> {
    match key {
        ConfigKey::Editor => &mut config.editor.default,
        ConfigKey::FileManager => &mut config.file_manager.default,
    }
}

impl ConfigKey {
    fn label(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::FileManager => "file manager",
        }
    }
}
