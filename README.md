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

## Session manager and guided creation

Run `dws` anywhere inside an initialized collection to manage existing sessions.
It opens on the launcher: a search field, large **New session**, **All sessions**
and **Commands** buttons, and your sessions with the most recently updated
first. An empty collection offers **Ctrl+N New session**. Create directly with:

```sh
dws new
dws new --editor "cursor --new-window"
dws new --no-open
```

Creation and Add use the same picker: type to filter repositories, **Ctrl+S**
toggles a normal link, **Ctrl+W** toggles a Git worktree, and **Enter** advances.
Worktree selections fetch `origin` and show a searchable branch picker; **Enter**
chooses a branch. New sessions ask for a name; Add keeps the existing name.
Escape clears a filter, backs up, or cancels the flow. **Ctrl+C quits the entire
command**, including from nested flows.

The TUI also supports the mouse: click a row to focus it, click its `[ ]` marker
to toggle selection, and double-click a session to open it. Double-click chooses
a branch in the branch picker. Scroll the list with the wheel, or click action
labels to open, create, add, rename, delete, show details, or cancel. Confirmation
still requires a separate deliberate click. Compact terminals shorten action
labels while keeping every session action available; F1 shows the keyboard map.
Mouse capture ends before an editor starts or the TUI exits.

**Ctrl+P** opens a searchable action palette, also available from the header.
Type an action name, use arrows or PageUp/PageDown, then Enter or click to run it.
Escape closes the palette without changing your session filter, selection, or
workflow input. Toolbar buttons show short action names; the palette and F1 list
their shortcuts. Delete still opens the existing confirmation.

In New/Add, the preview shows a truncated folder tree for the highlighted
repository. F2 also shows it in compact terminals. Discovery runs in the background
and is cached for the workflow: two levels, at most 64 lines, with `…` marking
omitted content. Symlinks are shown without following them; `.git`, `.dynws`,
`node_modules`, `target`, and `.venv` stay collapsed. File contents are never read.

New sessions open with an explicit `--editor` or the saved default. Automatic
editor detection is removed: configure your preferred workspace editor once:

```sh
dws config set editor "cursor --new-window"
dws open api-review
dws open api-review --editor "code --reuse-window"
```

Commands are parsed into executable and arguments and launched without shell
interpolation. The TUI restores the terminal before launch and waits for the
editor process, so terminal editors work too. A launch failure never removes a
successfully created session. `dws new --no-open` skips launch and prints the
workspace path after the interactive flow; it still requires a terminal.

## Create workspaces from manifests

Use `create` for scripts and agents. It never requires a terminal, prompts, or
opens an editor. Initialize the collection explicitly with `dws init` first.

```sh
dws create --file workspace.json
dws create --file workspace.toml --project /code --fetch --json
cat workspace.json | dws create --file - --input-format json --reuse
```

JSON and TOML share one schema. These definitions are equivalent:

```json
{
  "schema_version": 1,
  "name": "review",
  "repos": [
    {"path": "./web", "kind": "link"},
    {"path": "./services/api", "name": "service-api", "kind": "worktree", "branch": "origin/release"},
    {"path": "./tools/api", "name": "tools-api", "kind": "worktree", "new_branch": "advaith/fix", "base": "main"}
  ]
}
```

```toml
schema_version = 1
name = "review"

[[repos]]
path = "./web"
kind = "link"

[[repos]]
path = "./services/api"
name = "service-api"
kind = "worktree"
branch = "origin/release"

[[repos]]
path = "./tools/api"
name = "tools-api"
kind = "worktree"
new_branch = "advaith/fix"
base = "main"
```

Use `.json` or `.toml` extensions, or override detection with `--input-format`.
Stdin (`--file -`) requires `--input-format json|toml`. File repository paths
resolve relative to the manifest's containing directory; stdin paths resolve
from the caller's working directory. No shell or environment expansion occurs.
Repository paths are canonicalized. Ordinary links can target non-Git directories.

The schema requires version 1, an already valid workspace name and a nonempty
`repos` list. Unknown fields and duplicate canonical paths fail. Each repository
requires `path` and `kind`. Its optional `name` is the workspace link alias,
defaulting to the canonical directory basename; duplicate names require explicit
aliases. Links forbid branch fields. Worktrees require either `branch` or both
`new_branch` and `base`.

`--project PATH` selects an initialized collection root, ahead of `DYNWS_HOME`
and nearest-ancestor lookup. It does not initialize or repair the collection.
Relative `--project` and manifest filenames resolve from the caller's directory.

Existing local branches retain their exact identity. Local branches take
precedence over remote shorthand; use `refs/heads/...` or
`refs/remotes/origin/...` to disambiguate. Remote selections map to the local
branch name after the remote prefix; an existing local branch must track that
exact remote. Occupied branches fail without fallback or reset. `new_branch`
requires an absent local branch and a base resolving to a commit.

Local refs are used by default. `--fetch` fetches `origin` once per source Git
repository selected for worktrees, skipping links. It does not refresh other
remotes. Authentication failures return errors with credential/SSH prompts
disabled. Existing ref state without fetching is not a claim of remote freshness.

A workspace name collision fails by default. `--reuse` requires matching published
creation provenance: the same name, canonical inputs/sources, aliases, modes and
branch intent. Repository order and input format do not matter. Reuse verifies
all workspace links and actual Git identities. Dirty files, later commits and an
advanced base are allowed; the original resolved base commit remains recorded.
Legacy sessions remain manageable but lack the provenance required for strict reuse.
Targets left by failed creation cannot be adopted with `--reuse`.

Creation preflights the complete request and stages links and metadata. A
shared collection lock serializes creation and other session mutations. On failure, rollback removes
only newly created, verified worktrees and expected staging links. Existing
branches and unexpected files remain. Newly created branches are deliberately
retained and reported; inspect them before retrying, or revise the manifest to
explicitly select an existing branch. Dirty or otherwise unremovable worktrees
are retained with their paths and cleanup errors. Abrupt process termination
may leave staging files or targets; these are never automatically adopted.

Default success stdout is one absolute workspace path. `--json` and `--toml`
are mutually exclusive output options, independent of input format. They emit
one equivalent versioned result with `schema_version`, `name`, `path`, `status`
(`created` or `reused`), `repos`, and `warnings`. Each repository includes `name`,
`input`, `source`, `kind`, `path`, and applicable `branch`, `base`, and
`initial_commit` fields. Inapplicable optional fields are omitted. Diagnostics
go to stderr; errors exit nonzero without a success document.

## Manage sessions

In the launcher, type to filter and use the arrow keys to move. **Enter**
closes the manager and opens the highlighted session, even when other sessions
are selected. **Tab** switches to the full session list with a details sidebar.
**Right** displays repositories; **Left/Escape** returns. New sessions are built
in a dialog that previews the workspace, links and worktree paths before
anything is written.

| Key | Action |
| --- | --- |
| Ctrl+N | Create a new session |
| Tab | Switch between the launcher and all sessions |
| Ctrl+S | Toggle session selection |
| Ctrl+E | Rename one selected session, or the highlighted session |
| Ctrl+D | Delete selected sessions, or remove a repository link when expanded |
| Ctrl+A | Add links/worktrees to the highlighted or expanded session |
| Ctrl+R | Refresh after changes made elsewhere |
| Home/End, PageUp/PageDown | Navigate the active list |
| F1 / F2 | Full help / complete details and errors |
| Ctrl+C | Quit |

Selections persist across filters. The status shows hidden selections and delete
confirmation lists every target. **Enter, n, or Escape cancels deletion**;
`y` confirms. Bulk deletion validates and stages every target before committing;
staging failures restore the whole batch. Cleanup failures report retained paths.
Session deletion keeps source repositories and managed worktrees.

The slate interface adapts to terminal width and height. At 100×20 or larger,
a read-only preview shows the highlighted session or repository beside the list.
Medium terminals (at least 60×16) use aligned rows; compact terminals retain
names, selections, counts, and essential shortcuts down to 40×12. Below that
size, a resize message keeps cancellation and quit available. Resizing preserves
your filter, selection, highlighted item, and workflow progress.

Focused rows use a quiet dark surface and a pointer; selection markers remain
independent. F2 opens full scrollable details at every size. Long input keeps its
typed end and caret visible, and error feedback does not replace counts. During
branch fetching you can still filter and navigate repositories, cancel with Esc,
or quit with Ctrl+C; selection changes wait until fetching finishes.

Set `NO_COLOR=1` to use terminal-default colors and reverse/bold focus.
See the [responsive TUI verification](docs/verification/tui-polish.md) for
reviewed captures and local test coverage.

Quick commands work without a terminal:

```sh
dws edit api-review --name incident-review
dws edit incident-review --description "API and worker investigation"
dws edit incident-review --clear-description
dws reveal incident-review
dws remove incident-review
```

## Tab completion

Zsh and Bash complete commands, flags, configuration keys, manifest paths, and
live project-scoped session names. `dws open <Tab>` includes newly created
sessions immediately; rename and deletion are reflected on the next completion.
Completion never initializes storage, writes data, runs Git, or contacts a network.

Add the appropriate line to your shell configuration yourself:

```zsh
# ~/.zshrc, after Oh My Zsh / compinit
source <(DWS_COMPLETE=zsh dws)
```

```bash
# ~/.bashrc; works with macOS Bash 3.2 as well as newer Bash
eval "$(DWS_COMPLETE=bash dws)"
```

Generate the hook each shell startup: the pinned completion engine uses a
versioned, unstable integration protocol. No shell configuration is edited by dws.

`dws shell-init zsh` (or `bash`) prints the same completion hook plus a small
`dws` shell function, which is needed for `cd` below. Use it instead of the line
above:

```zsh
eval "$(dws shell-init zsh)"     # ~/.zshrc
```

```bash
eval "$(dws shell-init bash)"    # ~/.bashrc
```

## Migrating the command layout

| Previous invocation | Replacement |
| --- | --- |
| `dws` to create | `dws new` |
| `dws --editor CMD` / `dws --no-open` | `dws new --editor CMD` / `dws new --no-open` |
| `dws manage` | `dws` |
| `dws manage edit/remove/reveal …` | `dws edit/remove/reveal …` |
| `dws config editor set/clear …` | `dws config set/clear editor …` |
| `dws config file-manager set/clear …` | `dws config set/clear file-manager …` |
| `dws config file-manager list` | `dws config list file-manager` |
| `dws config editor list` | Removed; set an explicit editor command |

There are no compatibility aliases. Existing session/config files and validated
legacy adoption remain supported; no data migration is required. Manifest-based
`dws create` keeps its input/output contract.

## Worktree ownership and cleanup

Interactive worktrees are created only after an explicit `Ctrl+W` selection. `dws` fetches
`origin` (retrying without prune for ref-lock failures), records the selected
remote ref and actual local branch, and reuses a target only after verifying
that its repository and branch match.

Removing an individual worktree link from a session attempts Git cleanup only
when the worktree is owned by `dws` and no other session references it. Dirty,
locked, shared, legacy-unverified, and external worktrees are preserved; when
Git refuses cleanup, `dws` reports the retained path. Normal linked repositories
and source repositories are never deleted.

When deleting sessions containing extra workspace files or folders, dws lists
those paths in a second **Delete workspace files too?** dialog. Press **d** to
permanently delete them along with the sessions; **Enter**, **Esc** or **n**
cancels the entire removal. Folders include their contents; symlink targets,
source repositories and managed worktrees are kept. If the workspace changes
after review, deletion is refused and must be reviewed again. The interactive
`dws remove SESSION` command asks you to type `delete`; noninteractive removal
continues to refuse extra files.

## Configuration

```sh
dws config set editor "code --reuse-window"
dws config clear editor
dws config list file-manager
dws config set file-manager "gio open"
dws config clear file-manager
```

An explicit editor command takes precedence over the saved default. Neither
`EDITOR`/`VISUAL` nor a supported-editor catalog is consulted. The saved file
manager command is used by `dws reveal`; platform detection remains the fallback.

### Opening sessions

Press **Ctrl+O** in the launcher or session list to choose your configured editor
(Cursor when unset), T3 Code, or Finder (the configured desktop file manager on
Linux). Use **1–3** or arrow keys to choose, **Space** to remember that opener
for every session, and **Enter** to open. **Ctrl+T** opens directly in T3 Code;
both actions are also in the command palette.

The first T3 open shows the exact command and requires **y**; Enter cancels.
It uses `~/.t3/bin/t3 app <workspace>`, which creates a missing project and
opens it in the running desktop app. It never writes T3's database directly.
Errors stay visible; **F2** shows the command and output. Install T3's CLI from
the desktop app first. The app must be running and its local environment connected.

```sh
dws config set opener t3       # explicitly confirm T3 and make it the default
dws config set opener editor   # use your editor again
dws config clear opener        # restore the default editor behavior
```

The saved opener also applies to `dws open` and newly created interactive
sessions. An explicit `--editor` overrides it for that invocation.

By default dws starts the editor and waits for it, as a terminal editor needs.
Two switches change that for GUI editors such as Cursor:

```sh
dws config set background on   # start the editor detached; the shell is free at once
dws config set cd on           # move the shell into the workspace after opening
dws config clear background    # back to waiting
```

`cd` needs the shell function from `dws shell-init`: a program cannot change its
parent shell's directory, so dws writes the workspace path to a file the
function reads after dws exits. Without the function, dws prints how to add it.

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
