# CLI and TUI cleanup

Reviewed 2026-10-21. The design goal is a small session manager with a guided
creation/addition workflow. These references inform specific decisions; they do
not establish a ranking of command-line tools or justify copying whole features.

## Reference patterns

| Primary source | Observed pattern | Application to dynws |
| --- | --- | --- |
| [Worktrunk switch](https://worktrunk.dev/switch/) | A short explicit command and a picker when no target is supplied; new-branch intent is explicit. | Make plain `dws` the manager; keep `new` explicit and distinguish normal links from worktrees in the picker. |
| [Git worktree](https://git-scm.com/docs/git-worktree) | Worktree creation, listing, and removal have distinct semantics, with protections for unsafe removal. | Retain ownership checks and state exactly what session deletion or individual link removal removes. |
| [Jujutsu workspace](https://docs.jj-vcs.dev/latest/cli-reference/#jj-workspace) | `forget` leaves files on disk, while `remove` removes workspace files. | Explain deletion consequences before confirmation. Session deletion preserves source repositories and managed worktrees. |
| [GitHub CLI config](https://cli.github.com/manual/gh_config) | One configuration command family handles keys such as editor and browser. | Use shared `config set/clear` handling for editor and file manager. |
| [Ratatui List example](https://ratatui.rs/examples/widgets/list/) | A persistent `ListState` is passed to stateful rendering; a pointer and reverse highlight identify focus. | Preserve viewport state across frames in all lists and keep the focused row visible. |
| [Yazi keymap](https://yazi-rs.github.io/docs/configuration/keymap/) | Manager, input, confirmation, picker, and help contexts have their own controls. | Show short contextual shortcuts with complete F1 help. Preserve printable keys for filtering. |
| [fzf manual](https://github.com/junegunn/fzf/blob/master/man/man1/fzf.1) | Multi-selection, the current item, and matched items are distinct concepts. | Separate bulk selection from focus; opening always uses the highlighted session. Report selections hidden by filtering. |
| [Clap dynamic completion](https://docs.rs/clap_complete/latest/clap_complete/env/index.html) | Completion uses the command parser and customizable argument callbacks; generated hooks must match the executable's unstable protocol. | Pin `clap_complete`, generate hooks at shell startup, and read session names anew for each completion request. |

## Implementation decisions

The command parser was already small. The useful reductions were duplicate
launcher/configuration code and large decorative rendering functions. Editor
catalogs, automatic detection, aliases, and detected-editor listing were removed.
File-manager discovery remains because it supports the existing reveal action.
The stored session and configuration schemas remain compatible.

Creation and addition use the same repository picker, branch fetching,
materialization, validation, publication, and worktree rollback. Explicit flow
outcomes distinguish cancellation from quitting. Terminal teardown precedes an
editor launch, and the parent waits for that editor.

The terminal style translates the neutral dark palette into compact text, a
reverse focused row, and explicit selection markers. It retains Ratatui and
Crossterm. Layouts support 40×12 and show a resize message below that size.
Filesystem refresh and Git polling occur outside rendering.

Session mutations share the manifest command's project lock. Manager mutations
compare the loaded metadata under the lock before applying changes. Batch removal
validates every target, stages all removals, rolls back staging failures, then
commits logical deletion. Cleanup failure after commit retains and reports the
staged path. This handles ordinary errors; it does not claim crash recovery or
atomic visibility to readers that do not acquire the mutation lock.

Completion is intentionally cheaper than opening a session: it reads compatible
metadata and safe names without validating Git identities, executing Git, writing
a lock, or initializing a project. Operational commands still validate ownership.
The PTY acceptance script exercises actual Tab presses in isolated Bash and Zsh
sessions as well as session and worktree workflows.

Validation results and measurements are recorded in
[the verification report](../verification/cli-tui-cleanup.md).

## Responsive presentation refinement

The next presentation pass applies [Television's separate input, results and
preview regions](https://alexpasmantier.github.io/television/reference/channel-spec/)
through an automatic wide layout. [Yazi's semantic theme roles](https://yazi-rs.github.io/docs/configuration/theme/)
inform the distinction between names, metadata, selection markers and key hints.
[Lazygit's configurable focused rows and pane styling](https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md)
inform the quiet focused row and restrained divider. These are design adaptations,
not upstream requirements. The official Television homepage's interface image
was also visually inspected. dynws retains its own keybindings, monochrome palette,
font-independent markers and single-focus navigation.

Breakpoints are 100×20 for list/preview and 60×16 for padded/aligned presentation,
with 40×12 as the supported minimum. The preview uses cached state; it introduces
no commands, new persisted state, background scans or runtime dependencies.
