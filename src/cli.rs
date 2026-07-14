use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "dws")]
#[command(about = "Dynamic workspace manager for multi-repo IDE sessions")]
#[command(version)]
pub struct Cli {
    /// Editor command to use after creating/reusing a session in the interactive flow.
    #[arg(long, global = false)]
    pub editor: Option<String>,
    /// Do not open the session after the interactive flow completes.
    #[arg(long, global = false)]
    pub no_open: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Initialize dws for the repository collection rooted at the current directory.
    Init,
    /// Open a session workspace in an IDE/editor.
    Open {
        /// Session name.
        session: String,
        /// Editor command or known editor name to use for this launch.
        #[arg(long)]
        editor: Option<String>,
    },
    /// Edit or remove existing sessions.
    Manage {
        #[command(subcommand)]
        command: Option<ManageCommands>,
    },
    /// Manage dws configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    /// Manage the default editor.
    Editor {
        #[command(subcommand)]
        command: EditorCommands,
    },
    /// Manage the default file manager.
    FileManager {
        #[command(subcommand)]
        command: FileManagerCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum EditorCommands {
    /// List detected supported editor commands.
    List,
    /// Set the default editor command.
    Set {
        /// Editor command or known editor name.
        editor: String,
    },
    /// Clear the default editor command.
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum FileManagerCommands {
    /// List detected supported file manager commands.
    List,
    /// Set the default file manager command.
    Set {
        /// File manager command.
        file_manager: String,
    },
    /// Clear the default file manager command.
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum ManageCommands {
    /// Edit an existing session's metadata.
    Edit {
        /// Session name to edit.
        session: String,
        /// Rename the session.
        #[arg(long)]
        name: Option<String>,
        /// Set or replace the session description.
        #[arg(long)]
        description: Option<String>,
        /// Clear the session description.
        #[arg(long)]
        clear_description: bool,
    },
    /// Remove an existing session and its workspace folder.
    Remove {
        /// Session name to remove.
        session: String,
    },
    /// Reveal a session workspace in the OS file manager.
    Reveal {
        /// Session name to reveal.
        session: String,
    },
}
