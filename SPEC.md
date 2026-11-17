# dynws v0.1 Product Specification

## Product contract

`dynws` is a macOS and Linux CLI/TUI for creating named multi-repository IDE
sessions. The package is `dynws` and its only supported executable interface is
`dws`. A session workspace contains symbolic links to canonical source
repositories or to DWS-owned Git worktrees.

Initialization is an explicit boundary. `dws init`, help, version, and read-only shell completion are the
only interfaces available without a valid project. No other command may create
project storage as a side effect.

## Command surface

- `dws` opens the session manager on a launcher home (search, main actions,
  recent sessions), including an actionable empty state.
- `dws new [--editor COMMAND | --no-open]` runs guided creation, then opens the
  workspace unless launch was disabled.
- `dws open SESSION [--editor COMMAND]` opens an existing session.
- `dws edit SESSION [--name NAME] [--description TEXT | --clear-description]`
  transactionally edits metadata and paths.
- `dws remove SESSION` removes metadata/workspace links, retaining worktrees.
- `dws reveal SESSION` reveals a workspace in its saved or detected file manager.
- `dws config set <editor|file-manager> COMMAND` and `dws config clear
  <editor|file-manager>` manage saved launch commands.
- `dws config set opener <editor|t3|finder>` saves the opening destination;
  `dws config clear opener` restores editor opening.
- `dws config set <background|cd> <on|off>` and `dws config clear
  <background|cd>` control detached editor opening and shell directory changes.
- `dws shell-init <zsh|bash>` prints completion plus the directory-change wrapper.
- `dws config list file-manager` lists detected file-manager commands.
- `dws init` initializes, validates, or repairs the current collection.
- `dws create --file FILE [--input-format json|toml] [--project PATH] [--fetch]
  [--reuse] [--json|--toml]` retains the manifest creation contract.
- Help, version, and `DWS_COMPLETE=zsh|bash dws` completion work without a project.

`manage`, old setting-first config commands, and root-level creation flags are
removed without aliases. Stored formats remain compatible. There is no editor
catalog/detection: explicit launch command precedes the saved default, with
Cursor as the fallback. Open with selects a destination rather than detecting
editors. Foreground editor launches inherit stdio and wait after terminal
restoration; background editor launches detach with null stdio.

## Shell completion

Pinned Clap dynamic completion shares the parser's command/flag/enum definitions.
Existing-session positionals complete safe, filename-matching metadata names from
the current initialized project on every Tab, respecting ancestor lookup and
DYNWS_HOME. Malformed, unreadable and symlink records are skipped. Completion
never calls Git, accesses the network, initializes or mutates storage, or caches
session names. Actual operations still perform full validation. Shell startup
regenerates the hook; dws does not edit shell dotfiles. Both Zsh and Bash are tested.

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
  deletion. Staging failure rolls back the entire batch; post-commit cleanup
  failures are warnings containing retained staging paths.
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

## Noninteractive manifest creation

The manifest schema, path rules and command examples in README's "Create
workspaces from manifests" section are part of the public contract. JSON and
TOML use one strict typed schema. Creation does not enter the TUI or launch an
editor. Existing interactive fallback-branch and duplicate-session behavior
continues to apply only to the interactive flow.

Explicit project selection takes an initialized collection root; precedence is
--project, DYNWS_HOME, nearest ancestor. Input paths use the manifest directory
(or caller directory for stdin), independently of project selection.

Strict branch selection never resets, substitutes or forcibly checks out a
branch. Existing local branches precede remote shorthand; full refs disambiguate.
Remote selections require exact upstream identity. New branches require explicit
bases and record their initially resolved commits. Fetching is opt-in, origin-only,
once per source repository, with authentication prompts disabled.

New session metadata has optional version 1 creation provenance containing sorted
canonical input/source paths, aliases, modes, requested branch/new-branch/base
expressions, and initial base commits. Local-only worktrees have no remote_ref;
actual local_branch and verified source identity remain mandatory. Existing
metadata remains readable without fabricated remote refs or automatic migration.

Strict reuse requires same-name published provenance and exact intent equality,
ignoring repository order and the freshly resolved value of a recorded base.
Actual links, Git sources, branches and required upstreams must validate. Dirty
files and subsequent commits do not invalidate reuse. Legacy records and leftover
branches/targets cannot establish provenance.

Session mutations share an OS-backed collection lock in a permanent
create.lock file. Interactive mutations compare loaded metadata against the
manager snapshot under that lock; stale state fails with refresh guidance. All selections are preflighted before worktree creation. Links
and metadata are staged; final directory reservation and metadata publication
refuse replacement. Failure rolls back only invocation-created, identity-verified
resources. New branches, dirty targets and unexpected files are retained and
reported. Crash recovery is conservative: leftover state causes an actionable
failure, not automatic adoption. There is no claim of multi-file crash atomicity.

Output schema version 1 has name/path/status, repository input/source/kind/actual
path and branch, applicable base provenance, and warnings. Default stdout is only
the absolute path; JSON/TOML output stays free of subprocess/progress output.
Help requires no initialized collection.

## Interactive behavior and performance

The home screen manages sessions; Ctrl+N enters the same create workflow as
`dws new`. Ctrl+A enters the shared repository/worktree workflow for the
highlighted or expanded session, without asking for a new name. Cancellation
returns to the caller; Ctrl+C quits the entire command from every nested flow.

Printable characters extend the active filter. Up/Down, Home/End and PageUp/
PageDown navigate persistent stateful lists that keep the focused row visible.
Ctrl+S toggles a link or session selection; Ctrl+W selects a worktree. Enter
opens the highlighted session on home, accepts a branch, or advances creation.
Right expands repositories and Left/Escape returns. Ctrl+E renames one selected
or highlighted session; Ctrl+D removes selected/highlighted sessions or an
expanded repo link. Ctrl+R refreshes externally changed sessions. Escape first
closes overlays, clears filters, or returns to the preceding workflow step.

Ctrl+P or the header action trigger opens a searchable command palette populated
from the active view's shortcut definitions, including actions hidden by compact
layout. Search, scrolling and pointer selection are local to the palette; Escape
closes it without changing the underlying filter, selection or workflow input.
Enter/click dispatches the existing action handler after closing the palette.
Confirmation actions are excluded; Delete opens the normal confirmation. Below
minimum size execution is blocked, while Escape/quit remain available.

F1 opens scrollable help; F2 opens complete details/errors. Bulk selections
persist across filters, with hidden selection counts. Confirmation lists exact
targets and deletion consequences. Enter, n and Escape cancel; y or a deliberate click on Confirm confirms.
Repeated mutating input is not processed as overlapping operations.

Mouse capture is scoped to the terminal guard. Left-click focuses a rendered row;
its independent checkbox toggles selection. Double-click opens the focused
session or accepts a branch; bulk selection never changes the open target.
Wheel events navigate only the list or active scrollable dialog. Clickable action
labels use the same key handlers and safeguards. Hit targets are rebuilt from the
current layout and stateful list offset after every render; overlays exclude
underlying list/action targets. Rapid action clicks are coalesced for 350 ms,
including across a confirmation transition. Releases, drags and right-clicks
perform no action. Compact layouts drop shortcut annotations before action names.
Mouse capture is disabled on terminal teardown and setup failures.

The monochrome slate interface has compact header/filter/content/status/help
regions and supports 40×12 or larger. Below minimum size it shows resize guidance
and blocks mutation while retaining cancellation/quit. NO_COLOR uses default
terminal colors; textual markers preserve focus, selection, link/worktree and
missing states. No decorative animation or continuously redrawing idle screen.

An RAII guard restores raw mode, mouse capture and the alternate screen before editor launch,
on cancellation, and on errors. Input, resize, and active-fetch polling drive
redraws. Render methods perform no filesystem or Git operations. Repository
presence is cached on load/refresh/mutation, and fetch progress advances in the
event loop. Errors remain visible with full text available through F2.

New/Add repository previews include the active folder's cached tree, independently
of selection. One background worker scans at a time; navigating and resizing never
wait for it. Results update idle previews and already-open F2 details. The tree
shows two levels and at most 64 lines, inspects at most 129 entries per directory,
sorts sampled directories before files, marks depth/size truncation explicitly,
and never follows nested symlinks or reads file contents. Dependency/build and
metadata folders (`.git`, `.dynws`, `node_modules`, `target`, `.venv`) stay collapsed.
Empty/unreadable folders and loading have visible states. Cache lifetime is one
New/Add workflow; rendering never starts or polls filesystem discovery.

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

## Responsive terminal presentation

- At least 100×20: list and read-only focused-item preview, approximately 55/45.
- At least 60×16 below that threshold: padded single list with aligned metadata.
- At least 40×12 below that threshold: compact single list and essential controls.
- Smaller: resize message, cancellation and quit; no mutating actions.

Both dimensions are evaluated every frame. Resizing preserves filters, selections,
focus and workflow state. Lower-priority metadata truncates by terminal display
width; full content is available through F2. Input scrolling preserves graphemes
and keeps the typed end and caret visible. Default focus uses a dark raised row,
bold name and pointer. NO_COLOR retains terminal-default colors and reverse focus.

Session previews expose description, workspace and repositories; overflowing
repository summaries give a remaining count and the route to the full view.
Expanded-session previews follow the highlighted repository. F2 reuses the same
detail model and provides complete scrollable text. Counts describe the active
list and remain visible beside separate operation/error feedback.

Ctrl+O chooses the configured editor (Cursor when unset), T3 Code, or the
configured desktop file manager. Space toggles persistence; 1–3 and arrows
choose an opener; Esc cancels without saving. Ctrl+T invokes T3 directly.
The first T3 invocation displays the exact argument-vector command and requires
y; Enter cancels. `~/.t3/bin/t3 app <workspace>` owns project registration and
opening; dws never writes T3's database. GUI subprocess work stays off the render
thread, prevents overlapping actions, and exposes failures and output in F2.
Terminal editors launch after terminal teardown. Remembered openers apply to
CLI opening and interactive creation; explicit --editor wins. Config changes
from a stale Open with dialog are rejected before launch.

Dialogs use a bounded centered surface when space allows and the available
content region otherwise. They clear underlying content, retain complete
scrollable consequences/targets, and emphasize cancellation. Existing confirmation
and mutation semantics remain conservative by default. Session deletion with
extra workspace entries requires a second confirmation listing those paths:
d deletes the sessions and extra files/folders, while Enter/Esc/n cancels the
whole batch. Symlink targets and source repositories are kept. The reviewed
filesystem snapshot is revalidated before removal; changes require a new review.
Interactive CLI removal requires typing delete; noninteractive removal refuses
extra files. Visible busy feedback precedes expensive
work. Fetching remains cancellable and accepts source-list navigation/filtering,
while selection/publication actions are blocked until it completes. Rendering
and resize handling perform no discovery, filesystem checks or Git polling.
