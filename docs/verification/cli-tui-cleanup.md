# CLI and TUI verification

Date: 2026-10-21. Working-tree validation; no remote CI or publication is implied.
The concurrent manifest creation implementation is included in these checks.

## Baseline and measurements

The original review recorded 65 unit and 15 CLI tests. The first combined
implementation snapshot had 66 unit tests, with one manifest cleanup test failing
because a Git branch lookup retained leading whitespace. That concurrent fix is
present in the integrated suite. The final suite has 74 unit, 18 CLI, and 12
manifest integration tests: 104 tests total.

Line counts compare the implementation-start working snapshot, including the
then-current manifest work, against the final working tree. This avoids crediting
manifest additions as CLI cleanup. Counts include blank lines and comments.
For each `src/*.rs`, split at its trailing `#[cfg(test)]` / `mod tests` boundary;
the prefix is the implementation section and the suffix the inline test module.
Small test-only hooks inside an implementation section remain in its count.
Integration tests are counted separately under `tests/*.rs`. These are physical
line counts, not parser-derived production SLOC.

| Category | Before | After | Change |
| --- | ---: | ---: | ---: |
| Rust implementation sections | 6,489 | 6,297 | −192 |
| Inline Rust test modules | 2,005 | 2,421 | +416 |
| Rust integration tests | 1,149 | 1,303 | +154 |
| Direct runtime dependencies | 9 | 10 | +1 |

The additional runtime dependency is the pinned dynamic completion engine.
The 333-line Python PTY acceptance script is test infrastructure, measured
separately from Rust production and test code. Safety tests and completion add
necessary code while shared launch/configuration handling and compact rendering
remove duplication.

## Automated checks

The acceptance ledger is [GATES.cli-tui.md](../../GATES.cli-tui.md). Checks cover:

- Formatting, including explicit formatting of the included `src/lib.rs`.
- Strict Clippy with `-D warnings` and locked all-target tests.
- Release builds and verified crate packages on supported Rust toolchains.
- `python3 scripts/verify_terminal.py`: real PTYs, create/add/rename/delete,
  cancellation, worktree branch selection, resizing, editor terminal restoration,
  and actual command/flag/session Tab completion in isolated Bash and Zsh.
- Completion protocol tests: fresh names, rename/removal, nested directories,
  `DYNWS_HOME`, invalid metadata, quiet invalid projects, and no Git or writes.
- Batch removal preflight/staging failure, rollback, cleanup warnings, shared
  mutation locking, stale manager metadata, and worktree ownership protection.

| Local environment | Toolchain | Result |
| --- | --- | --- |
| macOS / Apple Silicon | Rust 1.88.0 | Passed tests, strict Clippy, release, package |
| macOS / Apple Silicon | Installed stable Rust 1.97.0 | Passed formatting, tests, strict Clippy, release, package, PTY suite |
| Debian Bookworm / Linux ARM64 in Podman | Rust 1.88.0 | Passed formatting, tests, strict Clippy, release, package, PTY suite |
| Debian Bookworm / Linux ARM64 in Podman | Stable Rust 1.99.1 | Passed formatting, tests, strict Clippy, release, package, PTY suite |

The Linux checks use the official Rust image, an isolated source fixture and
vendored locked dependencies. Tests never use the user's live sessions or Git
repositories. CI remains configured for Ubuntu/macOS × Rust 1.88/stable and now
runs the PTY acceptance script. That configuration is separate from an actual
remote CI result.

`cargo audit` exits successfully with an existing allowed unsoundness warning for
`lru 0.18.1`: [RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253).
This transitive version is unchanged from the repository baseline. The audit is
not a clean-advisory claim.

## Visual and interaction checks

Ratatui buffer captures were inspected at 120×40, 80×24, 60×16, 40×12, and 30×8.
Additional assertions exercise 100×28, 70×20, and 50×14. One hundred items are
exercised in session, repository, branch, and expanded-session lists. Tests cover
focus visibility, filtering, selection hidden by filtering, long names, complete
scrollable errors/details, help, and cancellation as the deletion default.

The inspected images are rendered TestBackend buffers, not native terminal
screenshots. Real PTYs independently exercise resize events and terminal control.
Focus and selection use a pointer/reverse row and textual markers; meaning is
retained with `NO_COLOR`. Below 40×12 the resize message retains cancellation and
quit. Review corrected compact picker counts and branch-specific visible totals.

## Compatibility and safety limits

Stored configuration and session metadata remain readable; the manifest input
and output contract is retained. Command spelling changes are intentional and
have no permanent compatibility aliases. See the [migration table](../../README.md#migrating-the-command-layout).

Batch removal provides rollback on ordinary staging errors. Cleanup errors after
logical commit retain and report paths. It does not claim recovery from process
termination or atomic observation by readers that do not take the mutation lock.
Session deletion retains source repositories and managed worktrees. Individual
worktree-link cleanup still requires ownership checks and Git's safe removal.
