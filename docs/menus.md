# Contextual menus

```text
statusbar tmux / prefix M-t
  Launch agent
    Codex / Claude Code / Trae / OpenCode (installed tools only)
      Single / Single Budget ───────────────────────────────────────┐
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
  frequent task commands: goal / side question / plan / compact (where supported)
  installed grill-me / handoff skills (prefill, no Enter)
  More commands
  Team members & progress (pane teams only)
    lead / workers + reported status → focus member
    restore main-vertical layout
  Launch agent
```

Each submenu keeps **← Back** (`B`) beside **× close** (`esc`) with right-aligned shortcuts in the footer on every page. Escape and outside clicks dismiss it. On
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
  `/compact` require idle. The main menu prioritizes `/goal`, `/side`, `/plan`, and `/compact`.
  Session branching, diff, status, model and permission controls live in More. Codex `/side` and `/btw` share the same handler; the menu uses only
  `/side` to match Codex terminology.
- Claude's catalog follows its [interactive-mode documentation](https://code.claude.com/docs/en/interactive-mode)
  and [command reference](https://code.claude.com/docs/en/commands).
  OpenCode uses its [TUI reference](https://opencode.ai/docs/tui/), including
  `/models` rather than Codex's `/model`. These are conservative built-ins,
  not a discovery mechanism for installed plugins or custom slash commands.
- TraeX 0.205.1's embedded command descriptions distinguish `/btw` (one-shot,
  no tools) from `/side` (an ephemeral fork). The main menu exposes `/goal`,
  `/btw`, `/plan`, `/compact`; the distinct `/side` action lives in More.
  Unrecognized CLIs get no guessed slash-command catalog.
- `grill-me` and `handoff` are pinned when a `SKILL.md` exists in the active
  tool's project or global skill directories. Project discovery stops at the
  worktree root. Codex/TraeX prefill `$name`, Claude prefills `/name`, and
  OpenCode prefills a request to use the named skill. Skills remain available while idle or working,
  never execute on menu selection, and retain the CLI's loading/permission
  checks. This is filesystem discovery, not an inventory of enabled plugins
  or per-session skill overrides.
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

## Single Budget

This preset opens one interactive CLI session, without team instructions or
worker configuration. The default Codex arguments select `gpt-5.6-luna` with
`high` reasoning effort. Other CLIs need explicit arguments so the menu does
not silently label their normal model as a budget model.

Override arguments in `$XDG_CONFIG_HOME/tmux-agent-workbench/launch.toml`
(default `~/.config/tmux-agent-workbench/launch.toml`):

```toml
[single_budget]
codex = ["--model", "gpt-5.6-luna", "-c", 'model_reasoning_effort="high"']
# Examples: choose model identifiers available to your account/provider.
# claude = ["--model", "sonnet"]
# traex = ["--model", "YOUR_BUDGET_MODEL"]
# opencode = ["--model", "YOUR_PROVIDER/YOUR_BUDGET_MODEL"]
```

An empty argument array disables that tool's budget preset. Values are argv
items, never shell snippets. Invalid configurations cannot launch, and known
noninteractive invocations are rejected. The preset shortcuts are Single `s`,
Single Budget `b`, Team `t`, Team Budget `T`.
