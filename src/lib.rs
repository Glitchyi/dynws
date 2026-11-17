mod cli;
mod completion;
mod config;
mod creation;
mod discovery;
mod editor;
mod git;
mod manifest;
mod opener;
mod session;
mod tui;

use crate::cli::{Cli, Commands, ConfigCommands, ConfigKey, Shell};
use crate::config::{Config, DynwsPaths};
use crate::editor::{
    detect_file_managers, launch, launch_detached, resolve_editor, resolve_file_manager,
};
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
    if let Some(Commands::ShellInit { shell }) = command {
        print!("{}", shell_init(shell));
        return Ok(());
    }
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
                open_session(&paths, &session, outcome.editor.as_deref())?;
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
            let names = [session];
            let review = store.review_removal(&names)?;
            let outcome = if review.extra_paths.is_empty() {
                store.remove_sessions(&names)?
            } else {
                require_terminal().context("extra workspace files need confirmation; run dws interactively to review deletion")?;
                eprintln!(
                    "Also permanently delete these workspace files/folders? Folders include all contents; symlink targets are kept."
                );
                for path in &review.extra_paths {
                    eprintln!("{}", path.display());
                }
                eprintln!(
                    "Type delete to remove the sessions and these files; anything else cancels:"
                );
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != "delete" {
                    bail!("removal cancelled");
                }
                store.remove_reviewed_sessions(&review)?
            };
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
        Some(Commands::Init | Commands::Create { .. } | Commands::ShellInit { .. }) => {
            unreachable!("handled before dispatch")
        }
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

/// Opens a session in the editor: in the foreground (waiting, as before) or,
/// with `open.background`, detached so the shell is free at once. With
/// `open.cd`, the workspace path is handed to the `dws shell-init` function,
/// which moves the shell there after dws exits.
fn open_session(paths: &DynwsPaths, name: &str, explicit: Option<&str>) -> Result<()> {
    let store = SessionStore::new(paths.clone());
    let metadata = store.load_session(name)?;
    let config = Config::load(paths)?;
    if explicit.is_none()
        && config
            .open
            .opener
            .is_some_and(|opener| opener != opener::Opener::Editor)
    {
        let opener = config.open.opener.expect("checked above");
        if opener == opener::Opener::T3 && !config.open.t3_confirmed {
            bail!(
                "T3 Code needs first-use confirmation; open dws and choose Ctrl+O, or confirm with 'dws config set opener t3'"
            );
        }
        let workspace = store.workspace_path(&metadata.name);
        opener::OpenPlan::new(opener, &config, &workspace, None)?.run()?;
        if config.open.cd {
            request_cd(&workspace);
        }
        return Ok(());
    }
    let selected = resolve_editor(
        explicit,
        config
            .editor
            .default
            .as_deref()
            .or(Some("cursor --new-window")),
    )?;
    let workspace = store.workspace_path(&metadata.name);
    if config.open.cd {
        request_cd(&workspace);
    }
    if config.open.background {
        launch_detached(&selected, &workspace)?;
        eprintln!(
            "opened {} with {} in the background",
            metadata.name,
            selected.command_line()
        );
        Ok(())
    } else {
        launch(&selected, &workspace, true)
    }
}

/// The shell function writes this file's contents to `cd` after dws exits.
const CD_FILE_ENV: &str = "DWS_CD_FILE";

fn request_cd(workspace: &std::path::Path) {
    match std::env::var_os(CD_FILE_ENV) {
        Some(file) if !file.is_empty() => {
            if let Err(error) = std::fs::write(&file, workspace.as_os_str().as_encoded_bytes()) {
                eprintln!("warning: could not hand the workspace to your shell: {error}");
            }
        }
        _ => eprintln!(
            "note: to cd into workspaces, add `eval \"$(dws shell-init zsh)\"` to ~/.zshrc (or `bash` to ~/.bashrc)"
        ),
    }
}

/// Shell integration: a `dws` function that runs the binary with a temporary
/// cd file and moves into the path it leaves behind, plus completion.
fn shell_init(shell: Shell) -> String {
    let completion = match shell {
        Shell::Zsh => "source <(DWS_COMPLETE=zsh command dws)",
        Shell::Bash => "eval \"$(DWS_COMPLETE=bash command dws)\"",
    };
    format!(
        r#"# dws shell integration ({name})
dws() {{
  local dws_cd_file dws_status dws_target
  dws_cd_file="$(mktemp "${{TMPDIR:-/tmp}}/dws-cd.XXXXXX")" || return
  DWS_CD_FILE="$dws_cd_file" command dws "$@"
  dws_status=$?
  if [ -s "$dws_cd_file" ]; then
    dws_target="$(cat "$dws_cd_file")"
    [ -d "$dws_target" ] && cd -- "$dws_target"
  fi
  rm -f -- "$dws_cd_file"
  return $dws_status
}}
{completion}
"#,
        name = match shell {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
        }
    )
}

fn configure(paths: &DynwsPaths, command: ConfigCommands) -> Result<()> {
    let _lock = if matches!(command, ConfigCommands::List { .. }) {
        None
    } else {
        Some(paths.mutation_lock()?)
    };
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
            if matches!(key, ConfigKey::Opener) {
                let opener = opener::Opener::parse(&command)?;
                config.open.opener = Some(opener);
                if opener == opener::Opener::T3 {
                    config.open.t3_confirmed = true;
                }
                config.save(paths)?;
                println!("default opener set to {command}");
                return Ok(());
            }
            if let Some(flag) = switch(&mut config, key) {
                *flag = parse_switch(&command)?;
                config.save(paths)?;
                println!(
                    "{} {}",
                    key.label(),
                    if *switch(&mut config, key).expect("switch key") {
                        "on"
                    } else {
                        "off"
                    }
                );
                return Ok(());
            }
            let selected = match key {
                ConfigKey::Editor => resolve_editor(Some(&command), None)?,
                ConfigKey::FileManager => {
                    resolve_file_manager(Some(&command), None, &detect_file_managers())?
                }
                ConfigKey::Background | ConfigKey::Cd | ConfigKey::Opener => {
                    unreachable!("handled above")
                }
            };
            *setting(&mut config, key).expect("command key") = Some(selected.command_line());
            config.save(paths)?;
            println!("default {} set to {}", key.label(), selected.command_line());
        }
        ConfigCommands::Clear { key } => {
            if matches!(key, ConfigKey::Opener) {
                config.open.opener = None;
                config.save(paths)?;
                println!("default opener cleared");
                return Ok(());
            }
            if let Some(flag) = switch(&mut config, key) {
                if std::mem::take(flag) {
                    config.save(paths)?;
                }
                println!("{} off", key.label());
                return Ok(());
            }
            if setting(&mut config, key).and_then(Option::take).is_none() {
                println!("default {} already unset", key.label());
            } else {
                config.save(paths)?;
                println!("default {} cleared", key.label());
            }
        }
    }
    Ok(())
}

/// The command-line settings; `None` for the on/off keys.
fn setting(config: &mut Config, key: ConfigKey) -> Option<&mut Option<String>> {
    match key {
        ConfigKey::Editor => Some(&mut config.editor.default),
        ConfigKey::FileManager => Some(&mut config.file_manager.default),
        ConfigKey::Background | ConfigKey::Cd | ConfigKey::Opener => None,
    }
}

/// The on/off settings; `None` for keys that store a command line.
fn switch(config: &mut Config, key: ConfigKey) -> Option<&mut bool> {
    match key {
        ConfigKey::Background => Some(&mut config.open.background),
        ConfigKey::Cd => Some(&mut config.open.cd),
        ConfigKey::Editor | ConfigKey::FileManager | ConfigKey::Opener => None,
    }
}

fn parse_switch(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        other => bail!("expected on or off, got {other:?}"),
    }
}

impl ConfigKey {
    fn label(self) -> &'static str {
        match self {
            Self::Opener => "opener",
            Self::Editor => "editor",
            Self::FileManager => "file manager",
            Self::Background => "background open",
            Self::Cd => "cd into workspace",
        }
    }
}
