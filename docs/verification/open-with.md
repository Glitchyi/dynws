# Open with verification

Verified 2026-11-17 on macOS arm64, with T3 Code (Beta) 0.1.7.

## Behavior

- Ctrl+O chooses the configured editor, T3 Code, or desktop file manager.
  Keyboard selection, mouse selection, remember, cancellation, and resize were
  exercised in disposable projects. Rendered buffers were inspected at 40×12
  and 80×24; layout assertions also cover 60×16 and 120×40. The local Menlo
  renderer lacks some bold box glyphs; this is a renderer artifact.
- First-use T3 confirmation displays the executable command; Enter cancels
  without launching or saving, and y confirms. Subsequent opens reuse the
  confirmation. Ctrl+T and the command palette use the same handler.
- Remembered defaults apply to `dws open`; an explicit `--editor` takes
  precedence. Editor choice restores the terminal before a foreground editor
  starts. Background editors remain detached. A real bash test covers shell cd.
- Stubbed T3 tests cover argument boundaries, repeated opens, nonzero exit and
  captured stderr, missing executable, and persistent settings. A fake desktop
  file manager covers its mouse selection and saved default.
- A stale preferences dialog is rejected before saving or launching. Config
  writes share the collection mutation lock.

## Live T3 evidence

The user approved registering and opening a scratch project. Tests used
`/private/tmp/dws-t3-smoke-20261117`; those scratch projects are retained.

1. `t3 project add <scratch> --title dws-opener-smoke` succeeded. Repeating it
   returned `ProjectAlreadyExistsError`, so preregistration is unsuitable for
   repeated session opening.
2. `t3 app <preregistered-scratch>` returned `project-create-failed`. Its cause
   was not established; the implementation preserves this error for F2.
3. `t3 app <fresh-scratch>/direct` succeeded, then succeeded again. The desktop
   activation code also creates a missing project before opening a thread.
4. The built `dws` initialized a scratch collection under `dws fixture`, created
   `opener-smoke`, saved `opener t3`, and successfully ran `dws open opener-smoke`.
   This verifies the actual integration with a workspace path containing spaces.

The implementation therefore uses only `~/.t3/bin/t3 app <workspace>`. It never
writes T3's database. The desktop app was already running; cold-start behavior
was not tested and the user’s app was not stopped.

## Checks

The test inventory is 99 unit tests, 21 CLI integration tests, and 12 creation
integration tests. Completion evidence is recorded in the local
`GATES.open-with.md` ledger: stable and Rust 1.88 tests, formatting including the
`autolib = false` module tree, strict Clippy on both toolchains, and the real PTY
verification script.

Linux CI exposed a zsh startup prompt for insecure completion directories on
the hosted runner. The shell fixture now audits a deliberately unsafe directory,
uses `compinit -i` to exclude unsafe paths, asserts that exclusion, and waits for
actual shell/hook readiness output before sending completion input. This changes
only the disposable test shell. The full PTY script passes locally with that
regression fixture; the subsequent CI run is the Linux verification source.
