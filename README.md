# dynws

`dynws` builds focused IDE workspaces from a collection of repositories. Its
`dws` command links normal repositories into a named session and can create
managed Git worktrees when a session needs a different branch.

`dynws` supports macOS and Linux. Native Windows is out of scope because the
workspace model relies on POSIX symbolic links; use WSL if you need to run it
on Windows.

## Requirements

- Git
- Rust 1.88 or newer when installing from source
- A terminal for the interactive create and manage flows

## Install

From crates.io:

```sh
cargo install dynws --locked
```

From a checkout:

```sh
cargo install --path . --locked
```

The package is named `dynws`; the installed executable is `dws`.

## Initialize a repository collection

Run `init` once at the root of the directory that contains your repositories:

```sh
cd ~/code
dws init
```

Initialization creates a versioned `.dynws/project.toml` marker and the
session, workspace, and managed-worktree directories. It is safe to run again:
`dws init` validates the project and repairs missing managed directories without
overwriting sessions or configuration.

Every other operational command requires an initialized project. From a child
directory, `dws` finds the nearest initialized ancestor. Set `DYNWS_HOME` to an
initialized `.dynws` directory to select one project explicitly. Missing or
invalid project state produces an error and is never created implicitly.

## Create and open sessions

Start the create flow anywhere inside the initialized collection:

```sh
dws
dws --editor code
dws --no-open
```

Type to filter repositories, use the arrow keys to move, press `Ctrl+S` to
toggle a normal link, and press `Ctrl+W` to toggle a Git worktree. Worktree
selections fetch `origin` and then provide a searchable remote-branch picker.
Press `Enter` to advance or create the session, `Escape` to go back or clear a
filter, and `Ctrl+C` to exit.

The resulting workspace opens with the explicit `--editor`, the configured
default, or an unambiguous detected editor. Use `--no-open` when you only want
the workspace path. Existing sessions are opened explicitly:

```sh
dws open api-review
dws open api-review --editor "code --reuse-window"
```

Editor commands may contain arguments and are executed directly, without shell
interpolation.

## Manage sessions

```sh
dws manage
dws manage edit api-review --name incident-review
dws manage edit incident-review --description "API and worker investigation"
dws manage edit incident-review --clear-description
dws manage reveal incident-review
dws manage remove incident-review
```

In `dws manage`, type to filter sessions and use the arrow keys to move.
`Ctrl+S` toggles session selection, `Enter` or Right expands a session,
`Ctrl+E` renames, `Ctrl+A` adds normal links or worktrees, and `Ctrl+D` removes
the selected item after confirmation. `Escape` backs out or clears the filter;
`Ctrl+C` exits. Printable letters always belong to the filter.

`manage edit`, `manage remove`, and `manage reveal` are non-interactive and can
be used from scripts. Removing a session deletes its metadata and workspace
links, but deliberately leaves its managed worktrees in place.

## Worktree ownership and cleanup

Worktrees are created only after an explicit `Ctrl+W` selection. `dws` fetches
`origin` (retrying without prune for ref-lock failures), records the selected
remote ref and actual local branch, and reuses a target only after verifying
that its repository and branch match.

Removing an individual worktree link from a session attempts Git cleanup only
when the worktree is owned by `dws` and no other session references it. Dirty,
locked, shared, legacy-unverified, and external worktrees are preserved; when
Git refuses cleanup, `dws` reports the retained path. Normal linked repositories
and source repositories are never deleted.

## Configuration

```sh
dws config editor list
dws config editor set "code --reuse-window"
dws config editor clear

dws config file-manager list
dws config file-manager set "gio open"
dws config file-manager clear
```

An explicit command-line editor takes precedence over the configured default,
which takes precedence over automatic detection. The configured file-manager
command is used by `dws manage reveal`.

## Project data

An initialized collection uses this layout:

```text
<collection>/.dynws/
├── project.toml
├── config.toml          # optional
├── sessions/
├── workspaces/
└── worktrees/
```

Session metadata records whether each repository is a normal link or a managed
worktree. Workspace directories contain links; the canonical source repository
remains outside DWS-managed storage.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md). `dynws` is available under the
[MIT License](LICENSE).
