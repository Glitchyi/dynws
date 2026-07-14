# dynws v0.1 Product Specification

## Product contract

`dynws` is a macOS and Linux CLI/TUI for creating named multi-repository IDE
sessions. The package is `dynws` and its only supported executable interface is
`dws`. A session workspace contains symbolic links to canonical source
repositories or to DWS-owned Git worktrees.

Initialization is an explicit boundary. `dws init`, help, and version are the
only interfaces available without a valid project. No other command may create
project storage as a side effect.

## Command surface

- `dws init` initializes or validates the current repository collection.
- `dws [--editor <command>] [--no-open]` interactively creates or reuses a
  session, then opens it unless requested otherwise.
- `dws open <session> [--editor <command>]` opens an existing session.
- `dws manage` interactively browses, filters, expands, edits, and removes
  sessions and their repository links.
- `dws manage edit <session> [--name <name>] [--description <text>]
  [--clear-description]` transactionally edits session metadata and paths.
- `dws manage remove <session>` removes session metadata and workspace links
  without implicitly deleting managed worktrees.
- `dws manage reveal <session>` reveals the workspace using the configured or
  detected file manager.
- `dws config editor <list|set|clear>` manages the default editor command.
- `dws config file-manager <list|set|clear>` manages the reveal command.
- `dws --help` and `dws --version` are always available.

There are no `setup`, `list`, `path`, shell-initialization, or zoxide commands.
The manage TUI does not open editors and does not contain an editor picker.

## Project and data model

An initialized collection has the following managed data:

```text
<collection>/.dynws/
├── project.toml
├── config.toml
├── sessions/
├── workspaces/
└── worktrees/
```

`project.toml` contains `schema_version = 1` and the canonical collection root.
Without `DYNWS_HOME`, resolution searches the current directory and its
ancestors for the nearest `.dynws/project.toml`. With `DYNWS_HOME`, that exact
directory must contain a valid marker and its recorded collection root controls
repository discovery.

`dws init` is idempotent. It creates the marker and managed directories on first
use and repairs missing managed directories on later runs without overwriting
valid data. A markerless legacy layout may be adopted only after all existing
config, session, workspace, and worktree state validates. Invalid or ambiguous
legacy state fails without mutation.

Session TOML records a safe single-component session name, optional description,
and uniquely named repositories with unique canonical paths. New repository
records have an explicit kind:

- `link` records a canonical external repository path.
- `worktree` records the canonical source repository, selected remote ref,
  actual local branch, and DWS-managed worktree path.

Legacy records without a kind are readable. They are considered DWS-owned
worktrees only after verifying managed-root containment, Git worktree identity,
common directory, source repository, and branch. Any unverifiable legacy record
is treated as an external link and is never deleted automatically.

## Safety invariants

- Loaded session filenames and internal names must agree. Names cannot be
  absolute, empty, `.`/`..`, multi-component, or otherwise escape managed roots.
- Session repository link names and canonical paths must be unique.
- Metadata and config writes use same-directory staging followed by atomic
  replacement.
- Session creation, repository addition, rename/edit, and removal publish
  metadata and workspace changes transactionally, with rollback on failure.
- Multi-session removal validates and stages every target before committing any
  deletion.
- Destructive workspace operations stop when unexpected real files,
  directories, or ownership mismatches are encountered.
- Source repositories, normal links, external worktrees, and shared worktrees
  are never recursively deleted.

Removing a session is conservative: metadata and workspace links are removed,
while managed worktrees remain. Removing an individual worktree link commits
the logical unlink first, then attempts `git worktree remove` only for an
unshared DWS-owned worktree. Dirty, locked, shared, external, or unverifiable
worktrees remain intact. Git cleanup failure is a warning that includes the
retained path, not a reason to corrupt the committed session state.

## Worktree lifecycle

1. `Ctrl+W` marks a Git repository for worktree creation.
2. DWS runs at most one owned fetch process at a time. It tries
   `git fetch origin --prune` and retries with `git fetch origin` only for the
   supported ref-lock failure.
3. The user filters and selects an `origin/*` branch.
4. DWS derives a lossless, path-safe worktree location and creates an available
   local branch, including a deterministic fallback when the requested branch
   is already checked out.
5. Existing targets are reused only after their source repository and branch
   identity validate.
6. The session operation tracks whether each worktree was created or reused.
   Rollback removes only worktrees created by that operation.

Cancellation and terminal teardown kill and wait for an active fetch child;
detached Git processes are not allowed.

## Interactive behavior and performance

Printable characters always extend the active filter. Arrow keys move,
`Ctrl+S` selects a normal link or toggles session selection, `Ctrl+W` selects a
worktree, `Ctrl+E` renames, `Enter`/Right expands or advances, `Ctrl+A` adds
repositories, `Ctrl+D` removes, Escape backs out or clears, and `Ctrl+C` exits.

Terminal state is protected by an RAII guard so normal exit, cancellation, and
errors restore raw mode and the alternate screen. Regular screens redraw only
for input or state changes; timed redraws are limited to an active fetch or
visible animation. Render methods perform no filesystem or Git operations.

Repository discovery excludes `.dynws`, produces deterministic ordering, uses
no more than eight workers, and obtains branch plus dirty-state enrichment with
at most one Git status process per candidate. Non-Git directories remain
selectable as normal links.

## Acceptance criteria

- Pre-initialization operational commands fail with actionable errors and leave
  no `.dynws` state; init, help, and version remain usable.
- Initialization, ancestor lookup, explicit `DYNWS_HOME`, legacy adoption, and
  partial-layout repair obey the project model above.
- Malicious or corrupt metadata cannot escape managed roots or cause external
  files, repositories, or worktrees to be deleted.
- Normal-link and worktree create/add/edit/remove workflows are transactional
  under injected failures.
- Worktree fetch fallback, cancellation, branch collision handling, validated
  reuse, rollback, sharing, dirty-state preservation, and legacy classification
  are covered by automated tests.
- Filters accept every printable letter, including letters formerly used as
  shortcuts, and idle render paths perform no Git or filesystem work.
- Formatting, strict Clippy, locked all-target tests, release builds, dependency
  audit, and package verification pass on Rust 1.88 and stable on Ubuntu and
  macOS.
