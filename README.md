# dynws

Dynamic workspace manager for opening a curated set of repositories in one IDE window.

The executable command is `dws`. It creates named workspace folders under `DYNWS_HOME` and fills them with symlinks to repositories you select from the current directory.

## Install from source

```sh
cargo install --path .
```

## Usage

```sh
dws
dws list
dws list --plain
dws list --editor code
dws path my-session
dws open my-session
dws open my-session --editor code
dws --editor code
dws --no-open
dws init zsh
dws zoxide sync
dws config editor list
dws config editor set code
dws config editor clear
```

`DYNWS_HOME` overrides the workspace root. When it is not set, `dws` uses a local `.dynws` folder in the directory where you run the command. This keeps generated workspaces near the repository collection so local git configuration context is preserved.

The interactive `dws` flow opens the resulting session in the configured default editor when one is available. Use `dws --editor <command>` to override that editor once, or `dws --no-open` to only print the session path.

`dws list` opens an interactive session picker when run in a terminal. Press `enter` to open the highlighted workspace in the default editor, `/` to filter, or `q` to quit. Use `dws list --plain` for script-friendly output.

Rust programs cannot change the current directory of the parent shell directly. Install the shell integration for a real `cd` workflow:

```sh
eval "$(dws init zsh)"
```

Use `dws init bash` for bash. For fish, run `dws init fish | source`.

After that:

```sh
dws-cd my-session
```

If `zoxide` is installed, `dws` automatically adds created/opened workspace folders to zoxide. You can also run `dws zoxide sync` to add all existing local dynamic workspaces, then use your normal zoxide command or the generated helper:

```sh
dws-z my-session
```

## TUI keys

- `j`/`k` or arrow keys: move
- `space`: select or unselect a repo
- `/`: edit fuzzy search
- `enter`: name/create the session
- `w`: create a git worktree for the highlighted repo
- `q`: quit

Session metadata lives in `<root>/sessions`, workspace symlinks live in `<root>/workspaces`, and generated worktrees live in `<root>/worktrees`, where `<root>` is `DYNWS_HOME` or the local `.dynws` folder.
