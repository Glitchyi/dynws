# Shared publication verification

The refactor routes manifest and picker creation through the same
`publication::publish` transaction. The session store acquires/retains the
collection lock and invokes the appropriate strict or interactive worktree
policy. `publication::Resources` tracks newly created worktrees and rolls them
back on failure; reused worktrees are never owned by that operation.

The prior strict collision fixture caught a staging-order regression during
extraction. Keeping strict materialization inside the prepared transaction
restored the contract: unexpected files injected during worktree creation stay
intact, and publication fails. The unchanged regression test passes.

A new transaction test also creates foreign metadata and an unexpected staging
file, then verifies refusal, preservation, and cleanup of only owned links.
Existing tests cover strict reuse/provenance, dirty worktree retention, retained
branches, locking, interactive duplicates, mixed creation, and rollback.

Creation warnings are returned to the CLI or printed after the interactive
terminal is restored. Add-to-session orchestration is unchanged. No existing
workspace or metadata migration, cross-flow reuse, new branch naming policy,
or crash-recovery guarantee is introduced.

The local `GATES.publication.md` records stable/Rust 1.88 tests, strict Clippy,
formatting and PTY workflow validation. Hosted CI is verified separately after
publication; a local pass alone is not evidence of hosted CI completion.
