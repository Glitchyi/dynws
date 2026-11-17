# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### OpenCode-style interface

- Adopt OpenCode's default dark theme: near-black page, raised panels, an orange
  focus bar, purple headings and lowercase key hints; `NO_COLOR` keeps reverse video.
- Empty collections open on a centered "dynws" logo home with a creation tip.
- Lists sit above a prompt box (filter input plus mode/status line) and a hint
  line ending in `ctrl+p commands`; wide terminals add a raised details sidebar.
- Help, details, rename, deletion and the categorized command palette are
  centered dialogs over a dimmed backdrop; deletion keeps Cancel as the default.
- Feedback appears as top-right toasts colored by outcome; branch-fetch progress
  moves into the prompt status line. Session rows show their last update age.

### Responsive terminal presentation

- Add a contextual Ctrl+P action palette with search, keyboard/mouse navigation,
  and preserved workflow/filter state; use existing handlers and confirmations.
- Simplify toolbar labels and add an always-discoverable header action trigger.

- Show a bounded, cached folder tree for the active repository in New/Add previews
  and F2 details. Scan off the event/render thread; mark omitted content and avoid
  following symlinks or expanding dependency/build directories.

- Add click-to-focus rows, independent clickable selection markers, double-click
  session opening/branch acceptance, wheel navigation and clickable action labels.
- Apply the Paneled design with slate frames, repository tree details and compact
  action labels. Keep mouse targets synchronized with resizing and list scrolling.
- Prevent dialog click-through and rapid repeated action clicks; disable mouse
  capture before editor launch and during terminal cleanup.

- Add focused-item previews in wide terminals, with medium and compact layouts
  that preserve workflow state during resizing.
- Align list metadata, introduce shared slate surfaces, retain an independent
  selection gutter, and keep long Unicode input and its caret visible.
- Separate feedback from counts, center clear confirmation/help/details surfaces,
  emphasize cancellation, and retain navigation/filtering during branch fetches.


### Added

- Session-manager home with shared New/Add workflows, live Zsh/Bash completion,
  stateful scrolling, compact monochrome layout, and full help/details views.
- Batch deletion with all-target staging/rollback, shared mutation locking and
  stale-manager checks; editor launches wait after restoring the terminal.

- Noninteractive `dws create` with equivalent JSON/TOML manifests, file/stdin
  input, explicit project selection, aliases and versioned structured output.
- Strict local/remote branch selection, explicit new-branch bases, opt-in
  noninteractive origin fetching and exact provenance-validated workspace reuse.
- Serialized creation with staged publication and conservative rollback that
  reports retained branches, dirty worktrees and cleanup failures.

### Changed

- Bare `dws` manages sessions; guided creation is `dws new`. Edit/remove/reveal
  are root commands and configuration uses shared set/clear actions.
- Creation-only flags moved under `new`; conflicting options fail during parsing.

### Removed

- The `manage` command hierarchy and old config syntax, without compatibility aliases.
- Automatic editor detection/catalog/listing; use an explicit or saved command.
- Decorative fetch animation and tinted status pills.

- Worktree metadata supports local-only branches without fabricated remote refs.
- Git worktree inspection handles quoted and multiline paths through NUL records.

## [0.1.0] - 2026-07-14

### Added

- Explicit, versioned repository-collection initialization with `dws init`.
- Interactive normal-link and Git-worktree session creation.
- Safe worktree fetch, branch selection, validated reuse, rollback, and
  conservative cleanup.
- Interactive session management plus scriptable edit, remove, and reveal
  commands.
- Configurable editor and file-manager command lines.
- macOS and Linux CI, packaged release archives, and SHA-256 checksums.

### Changed

- Operational commands now require a valid initialized DWS project and never
  create storage implicitly.
- Session and configuration mutations use validated, transactional writes.
- Filtering uses printable characters consistently; actions use explicit
  control and navigation keys.

### Removed

- Redundant `setup`, `list`, and `path` commands.
- Shell initialization and zoxide integration.
- Editor opening and editor selection from the manage TUI.

[Unreleased]: https://github.com/Glitchyi/dynws/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Glitchyi/dynws/releases/tag/v0.1.0
