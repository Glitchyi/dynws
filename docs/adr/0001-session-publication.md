# Share session publication while preserving creation policies

Date: 2026-11-17
Status: Accepted and implemented; local validation passed on 2026-11-18.
Base revision: `b6aeeae`.

## Accepted decisions

The accepted scope is:

- Preserve existing worktree paths and branch rules. Manifest creation remains
  strict; interactive creation retains verified worktree reuse and fallback
  branches. Do not migrate existing worktrees or metadata.
- Keep strict manifest `--reuse` eligibility unchanged. Making picker-created
  sessions reusable by manifests is a separate follow-up, requiring explicit
  provenance and branch-intent rules.

## Current problem

`creation::create` and `SessionStore::create_session_with_links` independently
stage links, publish metadata and workspaces, and recover from failure. Their
publication ordering and collision handling differ. The picker also tracks and
rolls back newly materialized worktrees in `tui.rs`.

The interactive add path already holds its collection lock across its stale
check, worktree materialization and mutation. This proposal improves locality;
it does not claim that path has an established unlocked race.

## Module and interface

The session store owns creation orchestration. Resolved metadata and worktree
requests supply the session name, description, links, optional existing creation
record, and the worktree operations required to materialize those links.

`publication::publish` implements the single staging/publication transaction.
`publication::Resources` tracks worktrees created by this operation and owns
rollback. `SessionStore::publish_strict_session` adapts strict manifest plans;
`SessionStore::create_session_from_choices` adapts picker choices. Strict
materialization runs after staging begins, preserving the existing collision
and rollback contract. Frontends do not implement a second commit protocol.

The module owns:

- The collection lock for the operation, validation before writes, and tracking
  which resources this operation created or merely reused.
- Staging links and serialized metadata; reserving the workspace name; refusing
  to replace an unexpected workspace or metadata file; publishing the workspace
  before committing the metadata that makes it a loadable session.
- Rollback of its owned staging files, links and newly created worktrees. Preserve
  dirty, changed, foreign and reused resources and report retention warnings.
- A structured outcome containing created/reused session metadata and warnings;
  adapters translate that into their existing output and UI states.

Manifest and picker adapters retain input parsing, branch-policy decisions,
fetch behavior and duplicate/reuse policy. They must not retain a second copy
of the staging/publication transaction. Strict and interactive Git operations
remain distinct implementations where behavior actually differs, without
inventing a public plugin system.

No new CLI options or metadata schema are proposed. Interactive sessions keep
`creation: None`; manifest sessions keep their validated creation record.
Existing user-facing duplicate prompts and non-overwriting strict behavior
remain intact.

## Implementation sequence

1. Extract the store-owned publication operation and resource tracking behind
   the resolved-plan interface. Move collision-safe staging and cleanup into it.
2. Adapt manifest creation, retaining its strict validation, target names,
   provenance, result formats and reuse checks.
3. Adapt interactive creation. Keep branch selection/fetch and duplicate prompts
   in the picker; remove its creation-specific transaction/rollback duplication.
4. Run the existing strict and interactive failure/rollback tests through this
   shared interface. Add focused tests only where the joined transaction exposes
   a missing failure scenario.

Adding repositories to an existing session, rename and removal retain their
current behavior. Move shared ownership bookkeeping only if required to remove
creation duplication; a broader intent-level mutation redesign is out of scope.

## Acceptance

- Both creation entrypoints publish through one store-owned operation.
- Existing paths, branch selection/fallbacks, duplicate prompts, strict reuse,
  metadata schema, result formats and editor behavior remain compatible.
- A concurrent/unexpected publication file is preserved and produces an error.
- Partial failure removes only owned, unchanged resources. Dirty/reused/foreign
  worktrees and created branches are retained according to current contracts;
  warnings reach the CLI or UI.
- Existing locking, reuse/tampering, rollback and interactive creation coverage
  passes, along with stable/Rust 1.88 tests, strict Clippy, formatting and PTY
  verification. Tests exercise the shared interface rather than a second
  implementation built only for tests.

## Limits

Workspace and metadata are separate filesystem publications. This refactor does
not promise a cross-file atomic transaction, power-loss durability, automatic
crash recovery, cross-flow reuse, one worktree layout, or a redesign of every
store mutation. The implementation must state its actual commit point and
preserve unexpected state during recovery from ordinary operation failures.
