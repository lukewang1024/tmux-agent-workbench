# Contextual menus

```text
statusbar tmux / prefix M-t
  Switch window → current Session's windows
  Switch session → Sessions only
  Switch agent → running Agents only
  Switch pane → current Window's panes
  New session → name, create in source directory, enter
  New window
  Launch agent
    Codex / Claude Code / Trae / OpenCode (installed tools only)
      One configuration panel (choices update in place)
        Codex / Trae: scene + Single/Team + Native/tmux (Team only)
        Claude / OpenCode: Single/Team + Native/tmux (Team only)
        Actual model / thinking / Fast preview; inline launch errors
        Enter: launch in source directory; Backspace: agents; Esc: close
  Panes & layout → split, zoom, main-vertical, tiled
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
```

All native menus use a shared footer. Submenus keep **← Back** (`Backspace`)
beside **× close** (`Esc`) with right-aligned shortcuts on every page. Key hints
use lowercase `enter`, `back`, and `esc`. Root menus only show Close; Backspace
also closes them without an extra hint. Escape and outside clicks always dismiss
the entire menu. On short terminals, `[` and `]` change pages. The source pane
stays fixed across
navigation, independent of the client's later active pane. Action callbacks
carry a hash of the target identity and rebuild their available actions before
executing. Window and launch menus check the pane, session and directory using
tmux metadata only. Agent commands additionally discover and verify the live
foreground process; changed targets or unavailable actions show a message.
Launch previews read local agent-team JSON settings directly, with no process
scan, Python startup, model lookup or cache warmup. Launch-time validation stays
in agent-team.

On tmux 3.8+, pointer and touch menus execute an item only on mouse release.
Holding a click never opens the next menu early, including configuration
changes and Back, so the same release cannot dismiss or activate that menu.
Older tmux keeps its pointer press-selection workaround because it mistakes
hover motion for a release; held-click navigation requires tmux 3.8+.

Known limitation, confirmed on tmux 3.8: clicking a disabled information row,
separator, title or border can still close the native menu. This is deferred;
Workbench does not require a patched tmux. Restoring `-O` would reintroduce
press-selection and the held-click submenu bug. Making information rows
selectable and reopening the menu would add keyboard stops and leave border
clicks unresolved. A complete Workbench-side solution would need a different
menu input implementation, with outside-click and keyboard behavior preserved.

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
- Codex and Trae scenes call `agent-team start TOOL --scene SCENE [--team] [--tmux]`; Claude and OpenCode call `agent-team start TOOL [--team] [--tmux]`
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

`tests/switcher.py` exercises real 40×20, 32×12, and 24×8 PTY clients, including
touching all three card rows, held presses, swipes, search, paging, keyboard-size
viewport changes, empty lists, disappearing targets, linked windows, multiple
clients, and Sidebar-to-list-to-Sidebar returns. The test uses local Agent
fixtures and does not call a model service. This is terminal-level simulation;
it does not claim a physical handset or Android keyboard acceptance run.

## Mobile navigation and interaction review

The four switch actions use filtered Sidebar views rather than tmux's tree or
fzf. They share Sidebar's popup container, title renderer, colors and secondary
context row; every cell in both entry rows is clickable. The bordered popup
uses about 90% of the client dimensions, capped at 96 columns by 40 rows, to
keep the terminal visible around it. Clients below 36 columns or 12 rows use
the available screen without a border. Sidebar's normal popup entry and these
filtered views follow the same sizing policy. Labels use all available cells
without adding an ellipsis; search matches their full, unshortened text.
The footer's four buttons occupy two full rows, with compact captions below
32 columns. Its previous/next controls remain visible alongside Back and Close.
Tap the search line or type `/` to search; pasted Chinese text is supported.
Arrow keys, Page Up/Down, Enter, Escape and Backspace remain available.

Sidebar and all four filtered views use `navigation_list::NavigationList`.
Views provide stable row keys, a row renderer, content bounds and action regions;
the shared component owns wheel/drag scrolling, release activation, keyboard
selection, hit testing, refresh anchors and resize handling. Wheel scrolling
moves the viewport independently of selection, including over pinned controls.
The same component also restores terminal mouse/focus/paste modes on exit.

| Mobile issue | Result |
| --- | --- |
| Native tree rows and dense fzf lists require precise taps | Sidebar's two-line entries provide a single touch target across both rows |
| Window/Pane scopes are ambiguous | Window stays in the opening Session; Pane stays in the opening Window |
| Navigation takes over the whole terminal | Sidebar and filtered views share a bounded popup; tiny clients use the full screen |
| Shortened labels spend cells on ellipses | Shared Sidebar labels use those cells for text; search still matches full titles |
| Press, swipe or refresh can select the wrong row | Release confirms a stable target identity; swipes and stale touches do not activate |
| Wheel behavior differs between lists or snaps back during refresh | All list views use one controller and preserve the browsed item through refresh |
| Sidebar opens a different picker and Back loses context | Both Sidebar routes use the same lists and preserve their origin |
| Keyboard reduces the terminal viewport | Entries, paging and footer resize together without clearing search |
| Short action menus reserve blank pagination rows | Menus that fit keep their full content; paginated menus use the actual footer budget |

Host/Agent/configuration actions still use the native menu
renderer. They retain single-row targets and the disabled-row dismissal behavior
documented above; moving these action panels to the shared touch renderer is the
next interaction improvement. The navigation lists themselves ignore blank-area
taps and expose explicit Back and Close buttons.
Returning to a popup Sidebar reselects the current Agent. Restoring its previous
browsing position, including scroll, remains a useful follow-up for long lists.

## Scene launch policy

Codex and Trae use only daily, hard and batch scenes, each in Single or Team mode.
Agent-team owns model, effort and Fast values. Workbench forwards the scene,
mode, with no independent model defaults or temporary adjustment selector.
Single `s` and Team `t` are the mode shortcuts. Scene shortcuts are daily `d`,
hard `h`, batch `b`; Native `n` and tmux `m` only appear in Team mode. All choices
stay in the same panel, and Enter launches with the current selection. Launch
is pinned with Back/Close even on short terminals. Launch and configuration errors
stay immediately above Launch on every page, survive pagination, and clear when
a choice changes or the panel is left. Fast is scene-wide in both
native and tmux modes, and is previewed from the actual agent-team config.

Claude and OpenCode have only Single / Team. No scene, Fast, quick or budget
selectors are shown, and the menu no longer reads `launch.toml` budget overrides.
Explicit scene/adjust/Fast options on the unified command are ignored with a
notice; explicit native CLI model options are preserved. Old standalone
agent-team budget commands remain available to existing scripts.

Trae Single, Team lead and worker use xhigh in every scene. Its batch scene
uses Luna 5.6; no Fast service tier is applied.

## Language and contextual controls

The launch panel follows the system display language: Chinese for `zh` variants,
English otherwise. On macOS it uses the first preferred system language, not
the input source or shell locale. Other systems use `LC_ALL`, `LC_MESSAGES`,
`LANG` and `LANGUAGE`, with English for C/POSIX. Tool names, model identifiers
and external diagnostics keep their original spelling.
`TMUX_AGENT_WORKBENCH_LANGUAGE=en|zh` explicitly overrides the display language
for a remote terminal or tests; one open menu retains its language across callbacks.

Agent startup, exit and suspension are refreshed by the existing detector.
The session label is the persistent Sidebar toggle. Wide clients toggle its
pane; narrow clients open the Agents panel. Session colours follow keyboard
mode: normal accent, prefix blue, OFF red.

The tmux root menu exposes Switch window (`w`), Switch session (`s`), Switch agent
(`g`), and Switch pane (`p`) first. New session (`n`), New window (`c`), Launch
agent (`a`), and Panes & layout (`l`) follow, then the installed editor and Detach.
There is no intermediate Switch submenu.
New session asks for a name and switches the invoking client to a new session
in the source pane's directory; cancelling the prompt creates nothing.

The left group runs Session/Sidebar → tmux → window labels.
CPU, Host and Agent status with usage stay on the right, whose outer
edge is flat. The theme's time/date/battery modules have independent opt-in
switches and stay visible in both densities when enabled. Clicking Session
toggles Sidebar. Prefix remains available through the keyboard. The
Agent status body opens commands for the current interactive foreground agent.
From a shell pane it focuses the most recently used agent in the same window
and opens its commands; without a remembered target, multiple agents are offered
in a single chooser. The target is checked again before opening commands, and
later callbacks stay bound to that pane. Usage has its own independent hit area.
Windows with no interactive agent hide the entire status/usage item.

Window demand now chooses density rather than a single client-width cutoff:
fixed controls compact first, then window names shorten, then tabs keep only
indices. The left cap adds no extra blank cells before the tabs. Sidebar visibility
does not affect the session label’s colours.

The theme compiles an ordered item list into native tmux formats. Visibility is
resolved before choosing each boundary from the previous visible background.
Separators and outer caps carry no mouse range; blank status areas do nothing.
Only actual window-tab ranges switch windows.
