use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::ArgValueCompleter;

#[derive(Debug, Parser)]
#[command(name = "dws", version, about = "Focused multi-repository workspaces")]
#[command(
    after_help = "Run dws for the session manager, or dws new to create a session.
Initialize a collection with dws init. Use dws create -h for manifest automation."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create a session with the guided repository/worktree picker.
    New {
        /// Editor command, including optional quoted arguments.
        #[arg(long, conflicts_with = "no_open")]
        editor: Option<String>,
        /// Create the workspace without launching an editor.
        #[arg(long)]
        no_open: bool,
    },
    /// Open a session in the explicit or saved editor.
    Open {
        #[arg(add = ArgValueCompleter::new(crate::completion::sessions))]
        session: String,
        #[arg(long)]
        editor: Option<String>,
    },
    /// Rename a session or change its description.
    Edit {
        #[arg(add = ArgValueCompleter::new(crate::completion::sessions))]
        session: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, conflicts_with = "clear_description")]
        description: Option<String>,
        #[arg(long)]
        clear_description: bool,
    },
    /// Remove session metadata and workspace links; retain managed worktrees.
    Remove {
        #[arg(add = ArgValueCompleter::new(crate::completion::sessions))]
        session: String,
    },
    /// Show a session workspace in the file manager.
    Reveal {
        #[arg(add = ArgValueCompleter::new(crate::completion::sessions))]
        session: String,
    },
    /// Set or clear saved launch commands, or list detected file managers.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Initialize or repair this repository collection.
    Init,
    /// Create a workspace from a JSON or TOML manifest without prompts or editor launch.
    #[command(after_help = "Examples:
  dws create --file workspace.json
  dws create --file workspace.toml --project /code --fetch --json
  cat workspace.json | dws create --file - --input-format json --reuse

Manifest: schema_version = 1, name, repos. Each repo has path, kind (link/worktree),
optional name (alias), and either branch or new_branch + base for worktrees.
Mix links and worktrees in repos. File paths are manifest-relative; stdin paths
are caller-relative. --fetch refreshes origin only. --reuse requires an exact
existing workspace. --json/--toml select output, independently of input format.

Mixed JSON definition (save as workspace.json):
  {\"schema_version\":1,\"name\":\"review\",\"repos\":[
    {\"path\":\"./web\",\"kind\":\"link\"},
    {\"path\":\"./api\",\"name\":\"backend\",\"kind\":\"worktree\",\"branch\":\"release\"},
    {\"path\":\"./worker\",\"kind\":\"worktree\",\"new_branch\":\"fix\",\"base\":\"origin/main\"}]}

TOML uses schema_version = 1, name = \"review\" and one [[repos]] table
per selection, with the same fields. Remote selections may use origin/release;
fully qualified refs/heads/... and refs/remotes/... disambiguate branch names.")]
    Create {
        /// Manifest filename (.json/.toml), or '-' to read stdin.
        #[arg(long, required = true)]
        file: std::path::PathBuf,
        /// Override the file extension; required for stdin.
        #[arg(long, value_enum)]
        input_format: Option<crate::manifest::InputFormat>,
        /// Select an initialized collection root (overrides DYNWS_HOME).
        #[arg(long)]
        project: Option<std::path::PathBuf>,
        /// Fetch origin once per source repository; never prompt for credentials.
        #[arg(long)]
        fetch: bool,
        /// Reuse only a published workspace with exactly matching intent and identities.
        #[arg(long)]
        reuse: bool,
        /// Emit a versioned JSON result.
        #[arg(long, conflicts_with = "toml")]
        json: bool,
        /// Emit the equivalent versioned TOML result.
        #[arg(long)]
        toml: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConfigKey {
    Editor,
    FileManager,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DetectKey {
    FileManager,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    /// Save a launch command.
    Set {
        #[arg(value_enum)]
        key: ConfigKey,
        command: String,
    },
    /// Clear a saved launch command.
    Clear {
        #[arg(value_enum)]
        key: ConfigKey,
    },
    /// List supported file managers detected on PATH.
    List {
        #[arg(value_enum)]
        key: DetectKey,
    },
}
