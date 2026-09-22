# Contextual menus

```text
statusbar tmux / prefix M-t
  Launch agent
    Codex / Claude Code / Trae / OpenCode (installed tools only)
      Single ───────────────────────────────────────┐
      Team / Team Budget                            │
        Native subagents (default) / tmux panes ─────┤
                                                    Start in source directory
  New window
  Panes & layout → split, zoom, main-vertical, tiled
  Switch → window, session
  Open project in VS Code (when installed)
  Detach this client

statusbar Agent / prefix M-a
  actual foreground CLI + state + source pane + team role
  common commands (prefill, no Enter)
  More commands
  Team members & progress (pane teams only)
    lead / workers + reported status → focus member
    restore main-vertical layout
  Focus this pane / Refresh / Launch agent
```

Each submenu has **Back** (`B`). Escape and outside clicks dismiss it. On
short terminals, `[` and `]` change pages. The source pane stays fixed across
navigation, independent of the client's later active pane. Action callbacks
carry a hash of the pane/process identity and rebuild their available actions
before executing; changed processes or unavailable actions show a message.

## Capability boundaries

- A process name in a pane title is not sufficient. Detection uses the existing
  manifest aliases and process ancestry, then requires a live foreground
  process group and an interactive CLI invocation. Background, suspended, or
  noninteractive jobs cannot receive slash commands.
- State uses the existing screen classifier plus a fresh daemon snapshot for
  the same process fingerprint. Visible approvals take priority. Unknown,
  blocked, and tmux copy-mode states hide input actions. Menus never start a
  model request, clear a draft, submit Enter, or answer an approval themselves.
- Codex's busy-safe subset follows the local upstream
  `codex-rs/tui/src/slash_command.rs::available_during_task`; `/fork` and
  `/compact` require idle. Common shortcuts include `/side`, `/btw`, `/status`.
- Claude's catalog follows its [interactive-mode documentation](https://code.claude.com/docs/en/interactive-mode)
  and [command reference](https://code.claude.com/docs/en/commands).
  OpenCode uses its [TUI reference](https://opencode.ai/docs/tui/), including
  `/models` rather than Codex's `/model`. These are conservative built-ins,
  not a discovery mechanism for installed plugins or custom slash commands.
- Trae distributions vary; the default entry is `/help` so that its own CLI
  supplies the version-specific commands. Unrecognized CLIs get no guessed
  slash-command catalog.
- Team presets call the public `agent-team TOOL [--team-budget] [--tmux]`
  interface. Workbench owns no model configuration or team execution policy.
  All launches are interactive; permissions inherit the CLI/team configuration.
- Pane membership is rechecked through tmux's `@agent_team` marker. Roles and
  status additionally require matching team ID, window, and socket in the XDG
  state file. If state is missing, live members remain navigable by pane ID.
  Member status is what the team last reported, not an inferred completion.
  Menus do not create workers or close an entire team implicitly.

## Validation

`sh tests/verify.sh` includes `tests/status-menu-mouse.py`. It runs an isolated
real tmux server and PTY client, exercising desktop and Termux menu behavior,
keyboard/status entrypoints, stale callback rejection, shell/approval/copy-mode
suppression, the launch wizard, source cwd, exactly one new window for Single,
native Team and pane Team Budget, member status and navigation, and narrow
terminal pagination. Fixture CLI executables and an `agent-team` boundary stub
avoid paid model requests; this validates menu orchestration, not model quality
or coding-agent authentication. Rust tests cover provider catalogs, busy-state
filtering, launch argument combinations, quoting, and unique shortcuts.
