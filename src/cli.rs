use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "dws")]
#[command(about = "Dynamic workspace manager for multi-repo IDE sessions")]
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
    /// List existing sessions and their linked repositories.
    List {
        /// Print the plain text list even when running in a terminal.
        #[arg(long)]
        plain: bool,
        /// Editor command or known editor name to use when opening from the list picker.
        #[arg(long)]
        editor: Option<String>,
    },
    /// Open a session workspace in an IDE/editor.
    Open {
        /// Session name.
        session: String,
        /// Editor command or known editor name to use for this launch.
        #[arg(long)]
        editor: Option<String>,
    },
    /// Print a session workspace path for shell cd helpers.
    Path {
        /// Session name.
        session: String,
    },
    /// Print shell integration for cd helpers.
    Init {
        /// Shell to generate integration for.
        shell: Shell,
    },
    /// Manage zoxide integration for dynamic workspaces.
    Zoxide {
        #[command(subcommand)]
        command: ZoxideCommands,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Subcommand)]
pub enum ZoxideCommands {
    /// Add all existing local dynamic workspaces to zoxide.
    Sync,
}
