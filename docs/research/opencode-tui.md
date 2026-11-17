# OpenCode reference: terminal polish

Reviewed 2026-10-21. Scope: interaction and presentation patterns for dynws's
existing session manager and shared New/Add workflow.

## Primary references

- [OpenCode TUI](https://dev.opencode.ai/v2/docs/cli/tui/): Ctrl+P lists available
  actions in the current view; typing filters action lists before execution.
- [Keybinds](https://opencode.ai/docs/keybinds/): command discovery coexists with
  direct shortcuts and separate session actions.
- [Themes](https://opencode.ai/docs/themes/): distinguish surfaces, text, focus
  and borders; retain terminal-default styling as a supported path.
- [Source](https://github.com/anomalyco/opencode): product reference, not imported
  implementation code.

The installed OpenCode v2.0.29 was checked in an isolated temporary HOME/XDG
profile, without loading this repository, credentials or sessions. The local
TUI remained at background-server startup because its service port could not
bind in that environment. This pass therefore uses documented interaction
patterns and dynws's actual render/PTY checks; it does not claim a complete
live visual comparison with OpenCode.

## Applied to dynws

1. Add one searchable action overlay on Ctrl+P, with a clickable header trigger.
   Register actions from the same contextual shortcut definitions used by the
   screen. Route execution through existing handlers so safety stays shared.
2. Keep toolbar action names and move detailed shortcut lookup into the palette
   and F1. Keep Quit/Back/Help/Info visible in the footer. No extra navigation mode.
3. Use consistent bounded dialogs, local search state, stateful scrolling and
   exact mouse hit regions. Escape closes only the palette; resizing, workflow
   inputs, session filters and bulk selection retain their state.
4. Preserve paneled slate styling, folder previews, NO_COLOR and minimum-size
   behavior. No theme catalog, leader-key system, slash-command parser, sidebar
   mode or additional runtime dependency.
5. Delete from the palette opens the existing confirmation. Confirmation itself
   is excluded from searchable commands; cancellation and underlying ownership,
   locking, rollback and stale-state checks stay intact. Pending fetches expose
   cancellation/help/details/quit rather than mutation actions.

The project-local `awwwards` skill is installed from `juliobsz/skills`, pinned to
`ea8f1455c2b0e71469d4ecfd32f47862b834d2d0`, with its upstream MIT license. It is
web-focused and was not applied as a website framework prescription to this TUI.
