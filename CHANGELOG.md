# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
