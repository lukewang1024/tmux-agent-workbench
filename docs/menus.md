# Contextual menus

```text
prefix M-m / tmux-agent-workbench menu
  Agent → current-window Agent commands, or an empty state and Usage
  Sidebar → overview, Switch session / agent / window / pane
  Tmux → window/session controls and agent launch
  Host / SSH → SSH destinations
  Extract → general text / paragraphs / URL / file paths / lines / words / quoted text
  CPU / resources → status density and btop
  Interface → classic tmux menus / new list menus

statusbar tmux / prefix M-t
  Create → Quick Agent / Agent / Window / Pane / Session
    Quick Agent → repeat last successful combination in the source directory
    Agent → installed tool → scene / mode / collaboration → Launch
    Window → new shell in source directory
    Pane → split below / right in source directory
    Session → name, create in source directory, enter
    Open project in VS Code (when installed)
  Switch → Agent / Window / Pane / Session
    Agent → running Agents only
    Window → current Session's windows
    Pane → current Window's panes
    Session → Sessions only
  Extract → the same text extraction submenu as the combined Menu
  Layout → zoom, even horizontal / vertical, main-vertical, tiled
  Kill → Agent / Window / Pane / Session
    Each shows its current target and requires confirmation
    Agent terminates the verified foreground Agent process only
  Detach → this client only

statusbar Agent / prefix M-a
  actual foreground CLI + state + source pane + team role
  frequent task commands: goal / side question / Codex fast (where supported)
  installed grill-me / handoff skills (prefill, no Enter)
  Usage summary (current CLI)
    Codex with codex-reset-card installed: Usage & reset cards
    Schedule earliest available card · 5 min before expiry (skill request)
    View / adjust scheduled tasks (skill request)
  More
    plan / compact / branching / status / configuration (where supported)
    Botmux (when installed)
  Team members & progress (pane teams only)
    lead / workers + reported status → focus member
    restore main-vertical layout
```

Extract uses the optional `tmux-extrakto` helper on the host, installed by the
[dotfiles profile](https://github.com/lukewang1024/dotfiles/blob/main/util/shell/tmux-extrakto).
Update that helper together with Workbench to enable the typed selections.
General text, paragraphs, file paths, lines, words/identifiers and single- or
double-quoted text default to Enter to copy and Tab to insert. URL uses Enter
to open on the invoking device and Ctrl-Y to copy. All choices retain the pane
and client that opened the menu; changing the active pane does not retarget them.
The same submenu is available in the classic and new menu renderers.

All action menus use `menu::Menu`, including legacy shell providers through the
`display-menu` adapter. Content providers supply labels, keys and callbacks;
two renderers consume the same model. The new list renderer is the default;
the classic native tmux renderer retains one-line menus with pagination and
requires explicit selection. Choose Interface in the
combined Menu, or run `tmux-agent-workbench ui old|new|toggle|status`, to switch
without reloading. The saved XDG configuration preference is overridden by
the current server's `@workbench-ui` option when it is set.
This setting changes action-menu rendering only. Sidebar and the Agent,
Window, Pane and Session switchers always use the shared list UI. Both menu
renderers expose the same ordered actions, targets and return destinations.

The new renderer owns layout, two-row buttons and pinned navigation. Its popup
fits the content, with a maximum width of 60 cells, rather than using Sidebar's
larger dimensions. Every cell of both button rows is clickable. Descriptions
wrap within the content area. Tapping outside the menu closes it on release
without sending that click to the underlying terminal. Outside hover, wheel
and drags leave it open, as do information, title and blank taps inside the
card. Escape and Close dismiss it; Backspace returns to its parent
or closes a root menu. Back, Close and Launch use full-width list rows, with
their shortcut on the right of the label, and stay pinned while the body scrolls.
Launch configuration summaries also stay visible near Launch when space allows.
The shared panel container captures a muted, in-memory snapshot of the terminal
behind its bounded card. This client-local input surface receives backdrop
clicks that native tmux popups discard. The live terminal returns on dismissal;
no pane, focus, or key binding is changed to implement outside dismissal.
Action menus dismiss when the terminal viewport changes. Navigation, searchable
lists and name dialogs retain search/input and scrolling when the phone keyboard
shrinks the viewport. If the client grows beyond the original input surface,
the panel dismisses so it cannot leave an uncaptured outside margin.
Horizontal rules separate the pinned header, scrolling body and footer in
Sidebar popups, switchers and new menus. Divider rows do not activate actions;
very short screens prioritize content and controls over divider rows.
Wheel and drag scrolling use the same `NavigationList` as Sidebar, and `[` / `]`
and Page Up/Down also browse a long menu. Host and member lists
are not limited by the number of available shortcut keys. The source pane
stays fixed across
navigation, independent of the client's later active pane. Action callbacks
carry a hash of the target identity and rebuild their available actions before
executing. Window and launch menus check the pane, session and directory using
tmux metadata only. Agent commands additionally discover and verify the live
foreground process; changed targets or unavailable actions show a message.
Launch previews read local agent-team JSON settings directly, with no process
scan, Python startup, model lookup or cache warmup. Launch-time validation stays
in agent-team.

New pointer and touch menus execute only on release. A hold, drag, wheel movement
or resize cannot launch a held item. The component handles these events itself
instead of depending on native `display-menu` mouse behavior. A selected
callback runs only after its popup closes, allowing it to open a submenu or
prompt. Private request/reply files carry the model and result and are removed
when the menu finishes. Usage source changes retain their invoking client and
parent menu.

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
  `/compact` require idle. The main menu prioritizes `/goal`, `/side`, and `/fast`.
  Planning, compaction, session branching, diff, status, model and permission controls live in More. Codex `/side` and `/btw` share the same handler; the menu uses only
  `/side` to match Codex terminology.
- Claude's catalog follows its [interactive-mode documentation](https://code.claude.com/docs/en/interactive-mode)
  and [command reference](https://code.claude.com/docs/en/commands).
  OpenCode uses its [TUI reference](https://opencode.ai/docs/tui/), including
  `/models` rather than Codex's `/model`. These are conservative built-ins,
  not a discovery mechanism for installed plugins or custom slash commands.
- TraeX 0.205.1's embedded command descriptions distinguish `/btw` (one-shot,
  no tools) from `/side` (an ephemeral fork). The main menu exposes `/goal`,
  `/btw`; planning, compaction and the distinct `/side` action live in More.
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
The PTY tests also cover classic/new round trips, the saved preference, native
pagination, SGR and legacy wheel reports on a fitting menu, and body/Close hover.

`tests/switcher.py` exercises real 40×20, 32×12, and 24×8 PTY clients, including
touching both card rows, held presses, swipes, search, paging, keyboard-size
viewport changes, empty lists, disappearing targets, linked windows, multiple
clients, and Sidebar-to-list-to-Sidebar returns. The test uses local Agent
fixtures and does not call a model service. This is terminal-level simulation;
it does not claim a physical handset or Android keyboard acceptance run.

## Mobile navigation and interaction review

In both menu modes the four switch actions use filtered Sidebar views. They share
Sidebar's popup container, title renderer, colors and secondary
context row; every cell in both entry rows is clickable. The bordered popup
uses about 90% of the client dimensions, capped at 96 columns by 40 rows, to
keep the terminal visible around it. Clients below 36 columns or 12 rows use
the available screen without a border. Sidebar's normal popup entry and these
filtered views follow the same sizing policy. Labels use all available cells
without adding an ellipsis; search matches their full, unshortened text.
The footer uses full-width Back and Close rows with right-aligned shortcuts;
short terminals reduce each button's height to preserve the list area.
Tap the search line or type `/` to search; pasted Chinese text is supported.
Arrow keys, Page Up/Down, Enter, Escape and Backspace remain available.

Sidebar and all four filtered views use `navigation_list::NavigationList`.
Views provide stable row keys, a row renderer, content bounds and action regions;
the shared component owns wheel/drag scrolling, release activation, keyboard
selection, hit testing, refresh anchors and resize handling. Wheel moves the
highlight, as the original Sidebar did, and reveals the selected row when
needed. Selecting the first entry resets the scroll offset to zero, including
its leading group heading. Wheel scrolling also reaches informational rows at
either end of the list. It works when every item fits on screen. Menu controls participate
in the same navigation, and hover highlights both entries and controls.
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
| Action menu rows are hard to tap | All menu providers use two-row, full-width targets and the shared list controller |
| Information taps dismiss a menu | Disabled rows and blank areas ignore taps |
| Status controls are too small to be the only entry | Prefix Alt+m opens the combined Agent/Sidebar/Tmux/Host/CPU menu |
| Long inventories are silently truncated | Host and member lists scroll beyond their shortcut-key count |

Host, Agent, launch configuration, Usage, Resources and Sidebar action menus
all use the shared Menu model. New navigation lists and action menus expose
explicit Back and Close buttons and share wheel/drag/release handling.
Returning to a popup Sidebar reselects the current Agent. Restoring its previous
browsing position, including scroll, remains a useful follow-up for long lists.

### Built-in modal audit

| Entry | Shared view | Touch dismissal |
| --- | --- | --- |
| Sidebar / responsive Sidebar | Sidebar list and panel surface | Outside tap or pinned Close |
| Switch Agent / Window / Pane / Session | Scoped Sidebar list and panel surface | Outside tap, Back or Close |
| Sidebar New / `pick project` | Searchable Menu with two-row items | Outside tap or pinned Close |
| Temporary tool / `pick repo` | Searchable Menu with two-row repository paths | Outside tap or pinned Close |
| `pick session` / `pick agent` / `agent goto` | Searchable Menu, retaining snapshot/tombstone behavior | Outside tap or pinned Close |
| New workspace / New task | Menu text input with Create | Outside tap or pinned Close; empty input cannot submit |
| Sidebar shortcuts and contextual/global actions | Menu model and the selected renderer | Outside tap, Back or Close |

Selection lists and name dialogs use the same touch UI in both action-menu
modes. Search accepts typed/pasted text; arrows, Tab/Shift-Tab and Page Up/Down
browse entries. Data is returned only after the popup closes, so a project
launch or tool never overlaps its picker. The old fzf wrappers remain available
for compatibility but no built-in selection entrypoint depends on them.
Repository selection shows the full searchable path across two rows; selecting
it still opens the configured Git/editor/shell tool. Those external terminal
applications, btop and interactive botmux commands retain their own UI.

## Scene launch policy

Codex offers daily, hard, batch and computer (电脑自动化) scenes, each in Single
or Team mode. Trae retains daily, hard and batch.
Agent-team owns model, effort and Fast values. Workbench forwards the scene,
mode, with no independent model defaults or temporary adjustment selector.
Single `s` and Team `t` are the mode shortcuts. Scene shortcuts are daily `d`,
hard `h`, batch `b`, and Codex computer `c`; Native `n` and tmux `m` only appear in
Team mode. All choices stay in the same panel, and Enter launches with the current selection. Launch
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

The tmux root menu exposes Create (`c`), Switch (`s`), Layout (`l`), Kill (`k`)
and Detach (`d`), in that order. Create's first item is Quick Agent (`q`),
whose gray subtitle shows the last successful menu launch's tool, scene,
Single/Team and Native/tmux choices. Either row starts a fresh conversation
in the source pane's current directory. The choices are saved atomically in
`$XDG_STATE_HOME/tmux-agent-workbench/last-agent-launch.json`; they are updated
only after a successful launch, and missing or invalid state disables Quick
Agent. Ordinary Agent creation remains available. All menu launches use each
CLI's automatic approval mode and explicitly disable the launcher's default
YOLO flags with `--no-yolo`.

Create then offers Agent (`a`), Window (`w`), Pane (`p`) and Session (`s`);
Switch and Kill start with those four objects. Back returns to the immediate
group, including after a scoped switch list or Agent launch configuration.
Kill shows the source target and a separate confirmation; its callback rechecks
the pane identity, and Agent termination additionally checks the process
fingerprint. It never uses a process-name match or kills the entire tmux server.
Create Session asks for a name and switches the invoking client to a new session
in the source pane's directory; cancelling the prompt creates nothing.

The overview Sidebar pins its Agents/sort heading above the scrolling body.
Its popup starts at the first group instead of following a lower current Agent
and hiding the first Session name. Persistent Sidebar panes retain their current
Agent selection. Changing sort explicitly starts the newly sorted list at its top.

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
