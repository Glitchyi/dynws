# dynws Specification and Agent Routing

## Summary

`dynws` is a Rust CLI/TUI for building dynamic workspace sessions. The executable command is `dws`. A session is a named folder under `DYNWS_HOME` that contains symbolic links to selected repositories, so one IDE window can open a curated multi-repo context.

Full v1 includes discovery, type-to-search multi-select TUI, session naming, duplicate detection, idempotent session creation, symlink population, session listing, editor launching, TOML config, git branch/status indicators, and optional origin-branch worktree creation under the resolved workspace root.

## Public Interfaces

- `dws`
  - Launches the interactive TUI from the current directory.
  - Lists immediate subdirectories, supports type-to-search fuzzy filtering, `Ctrl+S` normal selection, `Ctrl+W` worktree selection, session naming, duplicate reuse, and explicit duplicate override.
  - Opens the resulting session in the configured/default editor unless `--no-open` is passed.
- `dws list`
  - Opens an interactive existing-session picker in a terminal; pressing `enter` opens the highlighted workspace in the default editor.
  - Prints existing sessions and linked repository paths when piped or when `--plain` is passed.
- `dws path <session>`
  - Prints the workspace folder for shell `cd` helpers.
- `dws init <bash|zsh|fish>`
  - Prints shell functions that allow the parent shell to `cd` into dynamic workspaces.
- `dws setup [--editor <command-line>] [--file-manager <command-line>] [--yes]`
  - Initializes the local project `.dynws` layout.
  - Opens a TUI setup wizard in a terminal.
  - In non-interactive mode, requires `--yes` and uses provided flags/defaults.
- `dws zoxide sync`
  - Adds existing dynamic workspace folders to zoxide when zoxide is installed.
- `dws open <session> [--editor <name>]`
  - Opens the workspace folder with a configured or explicitly selected editor.
- `dws manage`
  - Opens an interactive existing-session manager for multi-selecting, expanding, renaming, opening, adding repositories to, or removing sessions.
  - Pressing `o` opens selected/highlighted workspaces in the default editor; pressing `Ctrl+O` opens a detected-editor picker for the same target.
  - Inside an expanded session, pressing `Ctrl+A` opens the repository picker and adds one or more normal symlinks or origin-branch worktrees to that session.
- `dws manage edit <session> [--name <new-name>] [--description <text>] [--clear-description]`
  - Updates session metadata. Renaming moves both the session TOML file and the workspace folder.
- `dws manage remove <session>`
  - Removes the session TOML file and workspace folder without removing linked repositories or generated worktrees.
- `dws manage reveal <session>`
  - Opens the session workspace in Finder, a detected POSIX opener, or the configured file manager.
- `dws config editor list`
  - Lists detected editor commands.
- `dws config editor set <name>`
  - Persists the default editor.
- `dws config editor clear`
  - Removes the persisted default editor.
- `dws config file-manager list`
  - Lists detected file manager commands.
- `dws config file-manager set <command>`
  - Persists the default file manager command for reveal actions.
- `dws config file-manager clear`
  - Removes the persisted default file manager command.

## Storage Layout

- `DYNWS_HOME` overrides the root folder; otherwise a local `.dynws` folder is created in the directory where `dws` is run.
- `<root>/config.toml` stores user config.
- `<root>/sessions/<session>.toml` stores session metadata.
- `<root>/workspaces/<session>/` stores symlinks to selected repositories.
- `<root>/worktrees/<repo>/<worktree>/` stores optional git worktrees created from the TUI.

## Implementation Modules

- `cli`: clap command definitions.
- `config`: path resolution, layout creation, TOML config loading/saving.
- `session`: session metadata, duplicate detection, idempotent creation, symlink behavior.
- `discovery`: current-directory subdirectory discovery with git status enrichment.
- `git`: portable `git` CLI wrappers for branch/status/worktree operations.
- `editor`: editor command detection, default selection, launch behavior.
- `tui`: ratatui/crossterm interactive create flow.

## Agent Routing

- Agent 1 owns scaffold, dependency wiring, CLI, and config behavior.
- Agent 2 owns session metadata, duplicate detection, idempotent creation, and symlink layout.
- Agent 3 owns ratatui screens, type-to-search filtering, selection, naming, branch prompts, duplicate prompts, and keybindings.
- Agent 4 owns git status/origin-branch worktree behavior and editor detection/opening.
- Agent 5 owns tests, README, and acceptance verification.

## Acceptance Criteria

- Running `dws` from a directory with subdirectories allows selecting repos and creating a named session.
- Repeating the same selected repo set reuses the existing session unless the user explicitly creates a duplicate with a different name.
- Session symlinks point to canonical original repository paths.
- `dws list` shows an interactive picker in terminals, opens the highlighted workspace on `enter`, and keeps plain linked-repo output for `--plain` or piped usage.
- `dws init zsh`/`bash`/`fish` enables `dws-cd <session>` in the parent shell, and zoxide-aware workflows can be synced with `dws zoxide sync`.
- `dws open <session>` uses `--editor`, then config default, then a single detected editor.
- `dws manage` can multi-select sessions, expand linked repos with `Right`, add normal links or worktrees with `Ctrl+A`, remove individual repo links, open workspaces in the default editor with `o`, and choose another detected editor with `Ctrl+O`.
- `dws setup` creates the local layout and can persist default editor and reveal/file-manager command lines.
- Configured editor and file-manager values may include arguments and are executed without shell interpolation.
- Individual repo-link removal deletes dws-created worktrees under `<root>/worktrees` with `git worktree remove`; non-dws linked repos are not deleted.
- Scriptable manage subcommands work without a TTY.
- Git repositories show branch and dirty status in the TUI; non-git folders remain selectable as normal repos.
- Pressing `Ctrl+S` toggles normal repo selection; pressing `Ctrl+W` toggles worktree selection for git repos.
- When worktrees are selected, the next stage shows an animated fetch screen while `git fetch origin --prune` runs in the background, falls back to `git fetch origin` on prune ref-lock failures, lists `origin/*` branches per repo, typing filters branches, and `Ctrl+S` selects the branch.
- Selected worktrees are created or reused under `<root>/worktrees/<repo>/<branch-slug>` and linked into the session under the repo name.
- `cargo test` passes.

## Assumptions

- POSIX-style systems such as Linux, macOS, and other Unix-like environments are supported targets.
- Windows-native support is intentionally out of scope because workspace sessions rely on POSIX symlink behavior.
- Windows users should use WSL or another POSIX-like environment.
- Session metadata and user config are TOML.
- The project uses the `git` command line rather than libgit2.
- Worktrees are created only through explicit `Ctrl+W` worktree selection and branch confirmation in the TUI.
- Repository addition is interactive-only, targets one expanded session, and supports multiple normal-link and worktree selections in one pass.
