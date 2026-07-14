# Contributing to dynws

Thank you for improving `dynws`. Changes should keep the command surface small,
preserve external repositories, and treat project metadata as untrusted input.

## Development setup

Install Git and Rust 1.88 or newer, then clone the repository:

```sh
git clone https://github.com/Glitchyi/dynws.git
cd dynws
cargo build --locked
```

Use a temporary repository collection when manually exercising `dws init` and
the TUIs. Do not initialize the source checkout unless that is the behavior you
are intentionally testing.

## Required checks

Run the same checks used by CI before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
cargo package --locked
```

CI runs these commands on Ubuntu and macOS with both Rust 1.88 and stable. It
also audits `Cargo.lock` for known vulnerabilities.

## Change guidelines

- Add tests for user-visible behavior and failure paths.
- Never infer worktree ownership from its path alone. Verify its Git identity,
  source repository, branch, and DWS metadata before cleanup.
- Keep session and workspace changes transactional. A failed operation must not
  leave half-published metadata, links, or worktrees.
- Treat session names, repository names, TOML paths, symlinks, and legacy state
  as untrusted input.
- Preserve normal source repositories, external worktrees, shared worktrees,
  and unexpected real files during destructive operations.
- Keep rendering free of filesystem access and subprocesses, and avoid adding
  new printable single-key actions that conflict with filtering.
- Update README.md, SPEC.md, and CHANGELOG.md when the public contract changes.

## Pull requests

Keep each pull request focused and explain the user-visible outcome, safety
considerations, and tests performed. The project uses squash merges so the pull
request title should be suitable as the final commit message.

By contributing, you agree that your contribution is licensed under the MIT
License.
