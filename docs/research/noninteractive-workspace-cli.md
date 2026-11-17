# Noninteractive workspace creation: CLI research

Research date: 2026-10-21. This is a design proposal, not an implemented command contract.

## Existing patterns

| Tool | Verified behavior | Useful pattern for DWS |
| --- | --- | --- |
| Git | `git worktree add <path> <branch>` uses an existing branch; `-b <new-branch>` explicitly creates one from a start point. `-b` refuses an existing branch; `-B` can reset it. Git normally refuses a branch already checked out elsewhere. | Distinguish selecting a branch from creating one; preserve collision safeguards. |
| GitHub CLI | `gh repo clone <repository> [<directory>]` separates source and destination; additional Git flags follow `--`. `gh repo create` supports interactive omission or noninteractive explicit name and visibility. | Keep the interactive entry point, add an explicit fully specified creation command. |
| Worktrunk | `wt switch <branch>` uses an existing branch; `--create <branch> --base <base>` explicitly creates a branch. Worktrees use computed paths, with `--path` override. | Keep branch names independent of generated storage paths; make new-branch intent explicit. |

Sources: [Git worktree manual](https://git-scm.com/docs/git-worktree), [GitHub CLI clone manual](https://cli.github.com/manual/gh_repo_clone), [GitHub CLI create manual](https://cli.github.com/manual/gh_repo_create), [Worktrunk switch manual](https://worktrunk.dev/switch/).

Git supports `worktree list --porcelain -z` for structured records with NUL termination. GitHub CLI offers `--json` on supported commands for scripts; Worktrunk provides `wt list --format=json` with a versioned schema. These establish a separate machine-readable output mode rather than requiring table parsing. Sources: [Git worktree output](https://git-scm.com/docs/git-worktree#_list_output_format), [GitHub CLI formatting](https://cli.github.com/manual/gh_help_formatting), [Worktrunk JSON output](https://worktrunk.dev/list/#json-output).

## Recommended DWS interface

The following syntax is a DWS-specific recommendation inferred from those patterns, not syntax copied from another tool:

```sh
# Ordinary links; no prompts, editor launch, or Git checkout changes.
dws create task --repo /code/api --repo /code/web

# Mix a normal link and a worktree on an existing explicit branch.
dws create review --repo /code/web --worktree /code/api release

# Create a branch explicitly from a base, independently for each repository.
dws create fix --new-worktree /code/api advaith/fix origin/main \
  --new-worktree /code/worker advaith/fix origin/main --json
```

Use repeatable fixed-arity flags: `--repo PATH`, `--worktree PATH BRANCH`, and `--new-worktree PATH NEW_BRANCH BASE`. Keeping a repository and its branch in the same argument group avoids positional coupling between independently repeated `--repo` and `--branch` lists. Avoid delimiter mini-languages such as `PATH:BRANCH` or `PATH=BRANCH`, since legal paths can contain those delimiters. Quote arguments containing spaces normally. Require a base for new branches to avoid silently choosing a repository-dependent default.

Alternative if a shorter flag set is preferred: repeat `--worktree PATH BRANCH` and reserve a separate future manifest for new branches. Do not overload the same branch token to mean either existing or new depending on whether it happens to exist.

## Behavior recommendations

These are proposed DWS decisions, not claimed guarantees of the compared tools:

- `dws create` never prompts. Missing or conflicting inputs return an actionable error. No `--yes` is needed for a command whose contract is already noninteractive.
- Retain explicit initialization and project discovery. An optional explicit project-root selector can make execution independent of the caller's working directory; it should validate existing initialization rather than initialize implicitly.
- Canonicalize repository paths, validate branch refs, duplicate inputs, link-name collisions, session collisions, and target ownership before mutations. Resolve relative paths from the caller's current directory.
- Existing local branch requests retain the exact branch identity. For explicit remote refs such as `origin/release`, document local tracking-branch naming. Fail when the requested local branch is checked out elsewhere unless a verified reusable target is explicitly supported. Never silently select a fallback branch or reset an existing branch.
- Start with local refs and make network fetch an explicit option. If fetching is enabled, disable Git credential prompts and report authentication failure instead of hanging automation. Do not infer that offline local refs are current remote state.
- Default stdout contains only the absolute workspace path. Progress and errors go to stderr. `--json` emits one documented object with schema version, workspace name/path, repository source/path/kind, actual branch, creation/reuse status, and warnings. Do not mix subprocess progress with JSON.
- Existing session names fail by default. Consider explicit `--reuse` only when the requested canonical repository/mode/branch set exactly matches persisted state; never silently append or overwrite on a retry.
- Validate all inputs before creating worktrees. Publish the session only after all required worktrees and links succeed. On failure, remove only worktrees created by that operation, retain reused targets, and report any retained paths. Avoid deleting newly created branches automatically without a separately designed safe policy.
- Preserve existing `dws` interactive behavior and conservative cleanup invariants. Add integration tests for mixed selections, non-TTY execution, quoted paths, exact branch selection, existing/new branch conflicts, JSON cleanliness, and rollback after a later repository fails.

## Fit with current implementation

The [current specification](../../SPEC.md) makes initialization explicit and session publication transactional. Its interactive worktree flow selects `origin/*` refs and permits deterministic fallback local branches when a branch is occupied. The new command should reuse transaction and ownership machinery while adding strict explicit-branch semantics. The existing `remote_ref` metadata requirement also needs review for local-only worktrees and branches created from local bases; do not fill it with a fictional remote ref. See [Git helpers](../../src/git.rs) and [session records and validation](../../src/session.rs).

## Decisions to confirm before implementation

1. Accept fixed-arity repeatable worktree flags, or prefer a structured manifest for mixed per-repository options?
2. Support new branches immediately, with explicit bases, or only existing local/remote branches initially?
3. Should creation use local refs by default with opt-in fetch?
4. Should a session collision fail, with exact-match reuse only behind an explicit option?

No code or runtime behavior was changed or tested as part of this research.
