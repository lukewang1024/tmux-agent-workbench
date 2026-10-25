# tmux-agent-workbench

tmux plugin for running agentic coding tasks (Claude Code, Codex, opencode)
inside tmux, where they actually live for hours or days instead of a single
terminal command.

Workbench v2 also discovers Codex, Claude, Trae, and OpenCode processes in
every pane of the current tmux server and maintains one canonical attention
snapshot. A per-server Rust daemon classifies `working`, `blocked`, `idle`, and
`unknown`, derives unseen `done`/blocked attention, drives a responsive sidebar
and touch-friendly searchable lists, and owns focus-aware desktop/sound notification delivery. It
does not require agents to be launched by Workbench and it never displays or
persists prompt text by default. See the durable
[v2 specification](docs/attention-v2.md) and
[implementation status](docs/attention-v2-status.md).

## What it is / why

A direction often has several tasks in flight. Workbench gives each task a
place to run and keeps their code changes independent:

- **One session groups a direction or larger goal.**
- **One window is one task**, with its own agent, conversation, and progress.
- **Each coding task owns isolated worktrees and branches.** Multiple tasks
  may use the same repository, but never the same writable checkout.
- Git tools, editors, shells, and services open on demand within the task.
  Temporary tools close on exit; there is no prebuilt inspection layout.

For example:

```sh
tmux-agent-workbench new --task formulas word-experience bear-web-x
tmux-agent-workbench task new image-layout bear-web-x
```

This creates two task windows in `word-experience`, with independent checkouts
under `~/Workspace/word-experience/tasks/formulas/` and `tasks/image-layout/`.
Tasks can start without repositories, and one task can span multiple repos.
Shared machine/environment maintenance is an explicit exception to checkout
isolation, rather than the default for parallel coding work.

The repo has two layers:

- **Layer 1** (`workbench.tmux` + `bin/`) tracks agents, supplies navigation,
  temporary tools, and supporting service panes.
- **Layer 2** (`git/workbench-git.tmux` + `git/bin/`) creates workspace sessions,
  task windows, and isolated worktrees using `~/Code` as the source-checkout
  pool and `~/Workspace` for task directories. Disable its tmux bindings with
  `@workbench-disable-git 1`.

Both layers live in one repo and load from a single `@tpm_plugins` entry —
see [Install](#install).

## Requirements

- **tmux >= 3.6 for interactive status/sidebar menus** (core requires >= 3.2), **git**, and the shipped `tmux-agent-workbench` binary —
  hard requirements. The installer downloads a matching macOS/Linux
  x86_64/aarch64 release, then falls back to a local Cargo build.
- **tig** or **lazygit** — used only when opening a temporary Git tool.
  Defaults to `tig`; set `@workbench-git-tool` to change it.
- **nvim** or **vim** — used only for an explicitly opened editor. Uses
  `$VISUAL`/`$EDITOR`, otherwise `nvim`, then `vim`.
- Project, repository, Session and Agent pickers use the built-in searchable
  list UI, with large touch targets and a pinned Close button.
- **zoxide** — soft dependency of the picker: widens its candidate list with
  your directory-jump history. Its absence degrades the picker (fewer
  candidates), it doesn't break it.
- **tmuxinator** and **sesh** — soft dependency of the *persistent pool*
  layer 2 generates. `tmux-agent-workbench projects rebuild` only ever writes YAML under
  `~/.config/tmuxinator/`; it never shells out to the `tmuxinator` binary
  itself. Whether that YAML actually gets used to spin up sessions later is
  on you (`tmuxinator start <name>`, or `sesh connect` if it reads the same
  pool) — install them if you want that part of the workflow, skip them if
  you're happy driving `tmux-agent-workbench new`/`tmux-agent-workbench add` directly.

## Install

Via TPM, one entry covers both layers — TPM discovers and runs every
executable `*.tmux` file under a plugin's cloned tree, so `workbench.tmux`
and `git/workbench-git.tmux` both load automatically:

```tmux
set -g @tpm_plugins 'lukewang1024/tmux-agent-workbench'
```

Then `prefix+I` to fetch and source it (or `prefix+U` to update, same as any
other TPM plugin).

Install the versioned attention binary once after TPM has cloned the plugin:

```sh
~/.local/share/tmux/plugins/tmux-agent-workbench/install
tmux-agent-workbench doctor
```

The POSIX installer first downloads the matching macOS/Linux x86_64/aarch64
release and falls back to `cargo build --release` when no release asset is
available. It installs executables under `~/.local/bin` by default, stages the
binary before replacing it, and signs the staged Mach-O on macOS. Reload tmux
after installation. Plugin loading never overwrites Workbench configuration or
Agent configuration. Termux is detected explicitly and always builds for its
native Android target. Tagged releases provide Android arm64 and x86_64 assets;
install the Termux `rust` package only when using an unreleased revision or an
architecture without a release asset.

The public CLI, tmux plugin, status menus and sidebar launchers share one core
selection rule: prefer the installed core over any binary left under
`target/release`. Thus a release upgrade also takes effect on machines
that previously built from source. To run a development build explicitly, set
`TMUX_AGENT_WORKBENCH_BIN` to that executable's absolute path; an invalid override
does not silently select another version. Without an installed
core, the checkout's release binary remains available as a fallback.

If you want layer 2 disabled from the start, set this **before** the
`@tpm_plugins` line so it's in effect the first time the plugin loads:

```tmux
set -g @workbench-disable-git 1
```

## Keybindings

Workbench uses the sidebar and compact Menu as its control surfaces. Press
`prefix+Tab` for Sidebar, or `prefix Alt+m` for the combined menu; once Sidebar
is focused, press `?` for a centered list of plain single-key actions.
Optional global bindings are (re)installed each time the
plugin loads when their option is explicitly set.

The classic tmux menu is the default. Choose **Interface · classic / new** in
the combined Menu to compare its two renderers. Buttons, order, scope checks
and callbacks are shared. The setting changes action menus only; Sidebar and
all four switchers always use the same list interaction. The CLI offers the
same switch:

```sh
tmux-agent-workbench ui old
tmux-agent-workbench ui new
tmux-agent-workbench ui toggle
tmux-agent-workbench ui status
```

The preference is saved under `$XDG_CONFIG_HOME/tmux-agent-workbench/ui-mode`.
The tmux option `@workbench-ui` (`old` / `new`) overrides it for one server.
The classic renderer uses native one-line tmux menus with pagination; the new
renderer uses larger list rows. Both modes use Sidebar-sized navigation popups
for Switch Agent, Window, Pane and Session.
Sidebar popups and all built-in selection lists close on an outside tap without
passing it to the terminal. Project/repository pickers and workspace/task name
dialogs use the same container and pinned Close control in either menu mode.

| Action | Option | Default | Runs |
|---|---|---|---|
| Pick/start a tmuxinator project | `@workbench-key-project` | unbound | Project picker |
| Open a temporary Git tool | `@workbench-key-inspect` | unbound | Task repository picker |
| Regenerate the tmuxinator pool | `@workbench-key-regen` | unbound | `tmux-agent-workbench projects rebuild` |
| Prompt for a new workspace | `@workbench-key-new` | unbound | Workspace name prompt |
| New isolated task in this workspace | `@workbench-key-task` | unbound | Task name popup |
| Toggle current-window Agent sidebar | `@workbench-key-sidebar` | `Tab` | Rust sidebar |
| Open combined Menu | `@workbench-key-main-menu` | `M-m` | Agent / Sidebar / Tmux / Host / CPU |
| Open tmux controls | `@workbench-key-mobile-menu` | `M-t` | Tmux menu |
| Open Agent commands | `@workbench-key-agent-menu` | `M-a` | Agent menu |
| Toggle all Agent sidebars | `@workbench-key-sidebar-all` | unbound | Rust sidebar |
| Pick any session by attention | `@workbench-key-session` | unbound | session picker |
| Pick any detected Agent | `@workbench-key-agent` | unbound | Agent picker |
| Jump to next unseen attention | `@workbench-key-attention` | unbound | attention queue |

The Agent sidebar/session/Agent/attention actions belong to layer 1 and do not
depend on the git-worktree layer. Regenerate/new/task require layer 2. The old `@workbench-key-promote` option
also opens the task-name popup for compatibility. Their
global options remain available for users who prefer direct prefix shortcuts;
the default avoids claiming memorable tmux keys for infrequent operations.

**Check for collisions before trusting any default, including these ones** —
a key that's free in your `tmux.conf` text can still be silently claimed by
a plugin loaded later via TPM (this exact thing happened choosing these
defaults: plain `g` looked free in the config file but `tmux-sessionist`
grabs it at load time). Verify with the *live*, post-plugin-load table, not
just the file:

```sh
tmux list-keys -T prefix | grep -E '(^| )(M-a|M-m|M-t|Tab)( |$)'
```

To rebind, set the option **before** the plugin loads (add it above
`@tpm_plugins` in `tmux.conf`, or set it and reload the plugin), e.g.:

```tmux
set -g @workbench-key-inspect 'g'
set -g @workbench-key-regen 'M-r'
set -g @workbench-key-new 'n'
set -g @workbench-key-task 'M-p'
```

Then `prefix+I` (or `tmux source-file ~/.tmux.conf` followed by re-running
the plugin script) to re-bind with the new key.

## Configuration

Read once at plugin-load time. Each option is bridged to a same-shaped
`WORKBENCH_*` environment variable **only if you actually set it** — if you
don't, the underlying scripts fall back to their own built-in default, so
"unset" and "set to the default value" behave identically.

| Option | Bridged env var | Default | Effect |
|---|---|---|---|
| `@workbench-agent` | `WORKBENCH_AGENT` | `claude` | Which CLI `tmux-agent-workbench agent start --managed` execs: `claude`, `codex`, `trae`, or `opencode`. |
| `@workbench-git-tool` | `WORKBENCH_GIT_TOOL` | `tig` | Git TUI opened on demand in a temporary popup (e.g. `lazygit`). |
| `@workbench-code-root` | `WORKBENCH_CODE_ROOT` | `~/Code` | Root of your persistent repo checkouts. Layer 2 looks here to resolve a short repo name (`tmux-agent-workbench add web-app`) to its main checkout. |
| `@workbench-workspace-root` | `WORKBENCH_WORKSPACE_ROOT` | `~/Workspace` | Workspace directions; task checkouts live at `<direction>/tasks/<task>/<repo>`. Legacy workspaces reopen as one existing task. |
| `@workbench-disable-git` | — (checked directly, not bridged) | unset | Set to `1` to skip loading layer 2 (`git/workbench-git.tmux`) entirely. |

```tmux
set -g @workbench-agent 'codex'
set -g @workbench-git-tool 'lazygit'
set -g @workbench-code-root '~/Code'
set -g @workbench-workspace-root '~/Workspace'
```

### Attention v2

The Rust engine reads
`$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml` (normally
`~/.config/tmux-agent-workbench/config.toml`). Unknown keys and invalid values
are rejected. `tmux-agent-workbench config check` validates the file and every
per-Agent manifest override; `tmux-agent-workbench reload` swaps a completely
validated replacement into the running daemon and retains the previous config
if validation fails.

```toml
[detection]
process_interval_ms = 1000
active_capture_interval_ms = 500
idle_capture_interval_ms = 2000
capture_lines = 40
capture_bytes = 65536
stale_grace_ms = 3000

[sidebar]
width = 26
min_width = 18
max_width = 36
main_min_width = 80
position = "left"
auto_create = true
agent_sort = "grouped" # grouped or prioritized

[notifications]
enabled = true
sound = true
style = "overlay" # overlay or system; Linux uses notify-send
volume = 1.0
no_repeat = true
mute_done = false
mute_request = false

[openpeon]
# packs_dir = "/path/to/packs" # default: ~/.openpeon/packs
# active_pack = "my-pack"

[relay]
bind = "127.0.0.1"
port = 19999
```

On macOS, clicking an overlay resolves the tmux client attached to the target
session, focuses its exact iTerm2 or Terminal TTY/tab, and then selects the
target window and pane. If no matching attached client exists, Workbench falls
back to the most recently active client and activates the terminal application.

The layout controller also honors the established tmux options
`@sidebar_width`, `@sidebar_position`, `@sidebar_auto_create`,
`@sidebar_min_width`, `@sidebar_max_width`, and
`@sidebar_main_min_width`. Explicit tmux values win over seeded config values.
All attention bindings can be changed with `@workbench-key-sidebar`,
`@workbench-key-sidebar-all`, `@workbench-key-session`,
`@workbench-key-agent`, and `@workbench-key-attention` before the plugin loads.

The sidebar shows all sessions and all detected agents from the current tmux
server in separate sections. A pinned top row keeps `new` on the left and
`menu` on the right. The Agents heading and sort control stay pinned below it.
Agents always appear above Sessions, on every screen size.
Session groups in Agents follow one another without blank rows. A single blank
row separates Agents from Sessions. Both sections use one continuous scrolling
list, without rebuilding or sorting it on resize. Help remains available from `?`
without taking space in the mouse action row. Session and Agent targets use
two-line cards so the full card is easier to click. The label at the right of
the Agents header toggles between `grouped`
(stable session/window/pane order) and `prioritized` (blocked, unseen done,
working, seen idle, unknown); the choice is remembered and synchronized across
all running sidebar instances. Press `d` to toggle a more detailed view with
Agent kind, human-readable window/pane indices, process id, state source, hook
health, and matched rule. Enter or left-click focuses a row; `m` or right-click
opens session/agent actions. The top row opens the tmuxinator-project picker and
the global action menu. In responsive popup mode, `Escape`, `Ctrl-C`, and
`Ctrl-D` close the popup; the footer and global menu also expose a mouse-driven
close action. Those exit keys are popup-only and do not terminate a persistent
sidebar pane. Activating a session or Agent with Enter or a left click also
closes the popup after the navigation succeeds.

Agent cards show the agent thread-name on the first line, falling back to the
tmux window title before a name is available.
The second line shows task progress (when available), workspace session name,
status, and agent type. Grouped mode shows the session name once above its
agents; prioritized mode keeps it on each agent card, so
parallel tasks within one direction remain distinguishable. Both lines are
clickable, and long Chinese titles truncate to their actual terminal width.

With focus in the sidebar, press `?` for the complete centered shortcut card.
The everyday keys are plain letters: `i` opens a temporary Git tool, `d` toggles details,
`n` jumps to attention, and `s`/`a` open pickers. Heavier mutating actions use
uppercase: `N` picks/starts a tmuxinator project, `W` creates a workspace, `T`
creates a task in the selected session, and `R` rebuilds generated projects. `m` opens the
row menu. The global menu exposes the
same management actions for mouse use.

The daemon publishes `@workbench_window_state` and
`@workbench_window_label` as window-scoped tmux options for status themes.
They aggregate Agents in that window using `blocked > working > unseen done >
idle`; both options are unset when the window has no Agent. This keeps process
detection in Workbench while allowing a theme to render a lightweight current-
window badge without polling or screen scraping.

When `tmux-adaptive-theme` is present, the plugin registers those values through
the theme's generic `@adaptive_context_*` interface. Agent Status opens the active agent's command menu. From a shell pane, it
focuses the most recently used interactive agent in the same window first;
without a remembered target, multiple agents appear in a single chooser.
The usage suffix has a separate click target for its usage menu. Windows
without an interactive agent hide this capsule. Separators and empty status
space have no click action. Usage shows the selected provider's remaining
quota and token totals from the local cached usage data; its existing compact-
width policy remains unchanged.

Host metrics are enabled by default in the adaptive theme Host capsule. The
resource capsule starts in Compact mode (CPU and memory); clicking it opens a
menu for Standard (also disk), Full (also upload/download), or btop. Workbench
owns sampling, caching, width defaults and the menu action. Set
`@workbench-host-metrics off` to disable them. See [host metrics](docs/host-metrics.md)
for mode selection, platform support and the standalone CLI.

Session/Sidebar and tmux controls form the left group, followed by window
labels. CPU, Host and Agent status remain on the right, with a flat
outer edge. tmux and Agent status open the same menus as `M-t` and `M-a`, while
the session label performs the same responsive Sidebar toggle as `Tab`.
Prefix remains available through the keyboard. Workbench owns their ranges and behavior;
the adaptive theme owns their placement, separators, and colors.
The Host segment is a separate clickable range: it launches the existing
`ssh-connect` effective inventory as a tmux menu; choosing a Host opens a new
window and connects there. Host filtering and SSH behavior remain owned by the
dotfiles command rather than duplicated in Workbench.

The tmux menu starts with **Switch**, **Create**, **Kill**, **Layout**, and
**Detach**. The first three groups start with **Agent**, **Window**, **Pane**,
and **Session**, in that order. Kill shows its target for confirmation and
rechecks its identity before acting. Switch Window lists only
the source Session's windows; Switch pane lists only its Window's panes. Sessions
and running Agents have separate server-wide views. In both menu modes these views share Sidebar's
bordered popup and two-line title/context rows, with search, paging, and a
full-width Back and Close rows. The popup leaves terminal context visible, caps its size
at 96 columns by 40 rows, and uses all available space on very small screens.
Labels use the available cells without ellipses. Selection happens on release;
dragging scrolls without activating. Sidebar's Session/Agent shortcuts and menu
use these same lists, and Back restores the originating Sidebar or tmux menu.
Wheel scrolling, touch, keyboard navigation and refresh positioning share one
list component across Sidebar and the four filtered views. New session prompts for
a name, starts in the source pane's directory, and switches only the invoking
client to the new session.
Selecting an agent opens one configuration panel. Codex and Trae expose scene,
Single/Team and (for Team) Native/tmux choices together; Claude and OpenCode
show only Single/Team and collaboration choices. Clicking a choice refreshes
this same panel. There is no temporary adjustment or separate review step.
All action menus share a footer: Escape closes the menu, and Backspace returns
one level when a parent exists or closes a root menu without an extra hint.
The launch panel keeps Launch visible on every
page; Enter launches. Shortcuts: `d/h/b` for scenes,
`s/t` for mode and `n/m` for Native/tmux.

The panel reads the actual model, effort and Fast preview from
[agent-team](https://github.com/lukewang1024/agent-team) without making a model
request. Errors appear inside the panel and preserve its selections. Workbench
does not duplicate model configuration; agent-team must be on PATH.

```sh
agent start codex --scene daily
agent start codex --scene hard --team
agent start codex --scene batch --team --tmux
agent start claude
agent start opencode --team
```

Workbench adds no approval-bypass flags. Single and native Team open one new
window; pane Team lets `agent-team` create its own window, with no launcher
pane left behind. Presentation is chosen at launch and never mixed mid-session.

The Agent menu includes **Usage summary** (`u`) to open plan limits and token
usage, including when the narrow status bar hides the usage badge.
The Host menu shows the current machine's hostname above its SSH destinations.

The Agent menu belongs to the pane from which it was opened. It identifies the
live foreground CLI from its process ancestry and offers provider-specific
commands: goal, side question, planning and compaction where supported.
Installed `grill-me` and `handoff` skills are pinned alongside them. Session
branching, status and configuration commands live under **More**. Shells, background or
noninteractive agents, approval prompts, unknown states, and tmux copy mode do
not receive slash commands. Working agents get only documented busy-safe
shortcuts. Commands **prefill without Enter** and do not erase an existing
draft; review the input in the CLI before submitting. Replacing the process
invalidates an open menu's callbacks. Reopening the menu rechecks status.

For pane teams, **Team members & progress** shows roles and member-reported
status from `agent-team`, lets you focus any live member, and restores the
main-vertical layout (`prefix Alt+4`). Native subagents remain managed inside
the coding CLI. Both the statusbar and `prefix M-a` / `prefix M-t` use the same
menu implementation, including touch behavior and small-screen pagination.
See [menu design and validation](docs/menus.md) for the interaction map and
capability boundaries.

`tmux-agent-workbench menu` and `prefix Alt+m` open a compact collection of
Agent, Sidebar, Tmux, Host/SSH and CPU/resources controls. Each submenu returns
to that collection. The source pane and client may also be supplied explicitly
with `--pane` and `--client`.

Status-bar menus open after `MouseUp`. Host, tmux, Agent, Usage, Resources and
Sidebar menus all use the same Menu model. In the new UI they have two-row buttons and a
content-sized popup capped at 60 columns. Menus share Sidebar's wheel/drag,
keyboard and release handling; held presses never open the next menu early.
Wheel moves the selection even when the list fits, and scrolls to keep it visible.
Hover highlights both entries and pinned controls. Back/Close and Launch remain
visible on short screens. Information and blank taps inside the menu leave it
open. Clicking the muted backdrop closes on release without clicking through;
outside hover, wheel and drags leave it open. Escape or Close also dismisses it.
Context menus dismiss when the terminal viewport changes; the scoped lists
retain their search and resize behavior.
Long Host/member lists remain scrollable
after their shortcut keys run out.

Set `@workbench-usage-source` to `codex`, `claude`, `trae`, or `opencode`
(default `codex`). Set `@workbench-usage off` before the plugin loads to omit
the usage suffix while retaining the Agent Status capsule and focus behavior.

The compact quota row displays the backend-reported plan type alongside the
remaining quota and reset time. Reset cards show the available count and the
nearest known expiry. No subscription multiplier is inferred from the plan name.

Usage refreshes in the background every ten minutes while the status bar is
rendering. Choose **Refresh now** (`r`) in the Usage menu to bypass the cache.
Codex plan limits come from `account/rateLimits/read` via `codex app-server`,
not session logs; this requires `codex` and Python 3 on the tmux server's PATH.
All windows share the cache under `$XDG_STATE_HOME/tmux-agent-workbench/usage`.
Failed quota requests retain the last successful reading with a `(stale)` label
in the Usage menu. Requests time out after twenty seconds.

Per-Agent manifest overrides live at
`$XDG_CONFIG_HOME/tmux-agent-workbench/manifests/{codex,claude,trae,opencode}.toml`.
One local file replaces that Agent's whole bundled manifest; Workbench does not
download rules. See [the accepted v2 specification](docs/attention-v2.md) for
the version-1 matcher and state semantics.

### Attention commands

```sh
tmux-agent-workbench daemon ensure|status|stop
tmux-agent-workbench snapshot --json
tmux-agent-workbench resurrect save|restore
tmux-agent-workbench agent explain %3 [--show-content]
tmux-agent-workbench metadata report --pane %3 --label build --ttl-ms 5000
tmux-agent-workbench config check
tmux-agent-workbench reload
tmux-agent-workbench doctor
```

`resurrect save|restore` is the all-in-one assistant session layer for this
plugin. Snapshot saving accepts both legacy tmux layouts and tmux 3.8 JSON
layouts, removes ephemeral sidebars, and writes a legacy layout that tmux can
restore across versions. Invalid conversions leave the original snapshot intact.
It composes with `tmux-resurrect` and `tmux-continuum`, preserves the
existing `assistant-sessions.json` data, restores Claude, Codex, OpenCode,
Copilot, Pi, Oh My Pi, and Grok sessions, and uses Workbench's native hooks for
Claude, Codex, OpenCode, and TraeX. No separate assistant-resurrect TPM entry
is needed.

Like Herdr, Workbench renders one state per Agent pane. Claude `/btw` and Codex
`/side` are classified from whichever thread is currently in the foreground;
main and side threads are not shown as separate rows.

Install the native lifecycle reporters explicitly after the binary is present:

```sh
tmux-agent-workbench hooks install all
tmux-agent-workbench hooks check all
```

Claude and TraeX JSON, Codex TOML, and the OpenCode XDG plugin are merged
idempotently. Existing hooks (including Peon Ping) and unknown settings are
preserved; `hooks remove` deletes only Workbench-owned entries. Hook processes
send only lifecycle metadata (event id, Agent/pane/session/process identity,
event type, timestamp, and reason category). They never scan transcripts or
deliver notifications. If the daemon socket is briefly unavailable, reports
are isolated by tmux server and spooled for at most 30 seconds.

There is one detached daemon per tmux server. It keeps scanning when every
sidebar is closed and exits when that server's socket disappears. Sidebar and
picker refreshes only read its versioned Unix-socket snapshot; they do not run
their own process scans, captures, or notifications.

Native hooks are the lifecycle authority: prompt/busy means working,
permission means blocked, and Stop/idle means done. A live Agent process exit
without Stop is the other authoritative completion source. Screen/title
matching remains a low-confidence estimate for hookless setups and lost-event
diagnosis; estimated transitions are dimmed and can never create attention or
notifications. Snapshot schema v1 keeps `base_state`/`display_state` and adds
`state_source`, `confidence`, `estimated_state`, and `hook_health`.

Codex automatic approval review displays as `working`, including when the pane
title says `Action Required`. The detector checks the live review overlay before
classifying that title. For hook-backed permission requests, the lifecycle state
is retained while the working display temporarily suppresses input attention;
a subsequent human approval prompt restores the blocked display and attention.

All outgoing channels use one notification pipeline: the state machine supplies
attention, `NotificationScheduler` rechecks it after one second, and
`NotificationPipeline` routes the resulting semantic event to a client or the
local/legacy-relay fallback. Automatic review suppression, cancellation,
expiration, and acceptance deduplication run before transport selection. A
channel switch cannot replay an accepted event. Client queues are revalidated
again before dequeue; obsolete relay retries are cancelled too. Restored
attention must agree with the reconciled live snapshot before it can notify.
Adapters handle platform rendering, framing, and transport retries only.

Workbench also consumes two non-lifecycle CESP signals without changing the
canonical pane state: `session.start` is silent on every channel, while native
failure events and failed `PostToolUse` payloads produce `task.error` through the
same pipeline without the attention debounce. The legacy HTTP relay protocol
continues to support only `task.complete` and `input.required`. Raw tool
payloads are inspected only inside the short-lived reporter and are never sent
to the daemon or persisted. OpenPeon categories are resolved from the active
pack, so a separate notification hook such as Peon Ping is not required.

### Client-aware SSH from Termux

Install the Termux:API Android app from the same signing source as Termux, then
install its CLI package and allow Android notification permission:

```sh
pkg install termux-api
tmux-agent-workbench client setup termux
termux-notification --title Test --content 'Workbench notification'
```

Use Workbench as the SSH entrypoint instead of attaching through plain `ssh`:

```sh
tmux-agent-workbench client attach devbox
tmux-agent-workbench client attach devbox --session my-task
```

The command keeps an authenticated control channel beside the interactive PTY.
Completion, input-required, and task-error events from the devbox are delivered
through `termux-notification`; tapping one brings Termux to the foreground and
uses that control channel to switch the attached remote tmux client to the
event's exact session, window, and pane. Closing the SSH attachment removes the
endpoint. No listening port, reverse tunnel, or third-party push provider is
required.

Reconnects restore the last session, window, and pane for that device and
terminal. An explicit `--session` takes precedence. History lives in the target
tmux server, survives Workbench daemon restarts, and expires with that server;
closed panes fall back to their window, then session. Plain SSH can invoke
`tmux-agent-workbench client attach` on the server: without a client identity,
it uses the server's last detached target as a shared fallback. With no sessions,
the local entrypoint creates one.

After responsive sidebar/zoom maintenance settles, Workbench refreshes the
client and sends one `SIGWINCH` to the active pane's foreground process group.
The same maintenance runs when the client switches sessions or windows.
If the window's recorded size and layout geometry disagree, maintenance reapplies
the recorded size and restores the original window sizing policy, so pane PTYs
receive the actual dimensions before the foreground process is notified.
This optional compatibility notification does not change terminal dimensions;
disable it with `set -g @workbench-attach-refresh off`. A signal being delivered
does not prove that an application's stale-rendering bug has been fixed.

Clients attached to the same tmux session share window selection and pane
dimensions. With `window-size latest`, the most recently active client determines
the window size; desktop navigation can therefore move and resize a phone's view.

Each local terminal has its own identity and attachment. Multiple Termux tabs
on the same phone can connect simultaneously. Reconnecting from the same
terminal replaces only that terminal's old remote client. A local terminal
lock prevents two Workbench connections from writing to the same terminal;
close the existing connection before starting another there. Notification
actions are scoped to the connection that received them.

Clients attached to the same tmux session still share its selected window.
Use `--session` with different sessions when independent navigation is needed.
This does not change tmux's shared pane layout or zoom semantics.

Upgrade the remote Workbench installation before upgrading clients: the Hello
message now includes an optional `terminal_id`. Updated servers accept older
clients without that field, but do not evict connections by device ID or infer
which old terminal they should replace. Older servers reject the new field;
the updated client cleans up that failed connection instead of falling back
to device-wide replacement. Both ends need updating for terminal-aware reconnects.

### Legacy SSH notification relay

Run one user-level listener on the laptop (under launchd, systemd --user, or a
terminal supervisor), then pair each SSH config host alias independently:

```sh
tmux-agent-workbench relay serve
tmux-agent-workbench relay pair devbox
tmux-agent-workbench relay doctor devbox
```

`pair` prints the required SSH stanza, for example
`RemoteForward 19999 localhost:19999`; it never edits `~/.ssh/config`.
`relay doctor` checks the alias and probes that reverse tunnel from the remote.
Use `relay rotate devbox` to atomically replace its token and
`relay revoke devbox` to remove the pairing. Tokens are stored mode 0600.
The listener accepts only authenticated CESP `task.complete` and
`input.required` events at `POST /v1/events`, with a 16 KiB limit and rate
limiting. Notification clicks map the stored remote id back to the SSH alias
and execute only Workbench's validated fixed focus command. On macOS the click
first focuses the interactive terminal tab running that SSH host, then switches
the matching remote tmux client to the exact target window and pane. Remote
arguments are POSIX-shell quoted before OpenSSH transports them, so opaque tmux
ids such as `$11` are not expanded by the remote login shell.

### Migration from tmux-agent-sidebar

Remove the old `tmux-agent-sidebar` TPM item, hook installer/updater calls,
hooks containing its path, and obsolete rich-UI options before enabling v2.
Do not load both plugins: Workbench refuses to create a second sidebar, and
`tmux-agent-workbench doctor` prints the remaining cleanup steps. V2 does not
modify Claude, Codex, Trae, or OpenCode hook/config files; optional metadata is
reported through the TTL-bound command above.

## Commands

### Public workflow API

All callers use `tmux-agent-workbench`. `pick project` opens the tmuxinator
project picker; `pick session` opens the running-session picker. The dispatcher
owns the mapping to private `wb-*` implementations. Legacy aliases only forward
to this public API and are retained for compatibility through 2.1.

### Layer 1 — repo-agnostic (`bin/`)

- **`tmux-agent-workbench agent start --managed`** — launches the current task's
  agent (`$WORKBENCH_AGENT`: `claude`, `codex`, `trae`, or `opencode`). New task
  windows launch this automatically. Agent/profile pane metadata supports handoff.
- **`tmux-agent-workbench inspect <path> [--git|--editor|--shell] [--pane]`** — opens
  one temporary tool, in a popup by default or a supporting pane with `--pane`.
  It creates no window and no fixed layout. Exit the tool to close it. Legacy
  `--focus`/`--force` flags are accepted without recreating inspection windows.
- **`tmux-agent-workbench pick repo [--git|--editor|--shell]`** — selects a directory
  for a temporary tool. Within a task it lists that task's checkouts; elsewhere
  it uses directory history and configured project roots. It does not add repos
  or send commands to another agent.
- **`tmux-agent-workbench run [--name <label>] <own-checkout> -- <argv...>`** — opens
  a persistent service in a supporting pane of the owning task's window.
  `--shell '<command>'` supports shell syntax. Repeating a name replaces that
  task's service, while another task's same-named service remains independent.
  The pane closes on command exit. One-shot agent commands use execution tools.
  Configured proxy variables are passed into actual tmux-launched processes in
  both uppercase and lowercase forms.
- `add`, `run`, and temporary tools target the calling pane's task. For a caller
  without pane ancestry, provide verified `TMUX_PANE`, or set
  `WORKBENCH_SESSION` and `WORKBENCH_TASK` explicitly. With multiple tasks, a
  session alone is ambiguous. Another task's worktree is never selected merely
  because it shares a Git common directory.
- **`tmux-agent-workbench agent handoff --target <profile> < summary`** — hands the current task to a
  fresh coding-agent profile in a detached right-hand pane, using the target
  CLI's initial-prompt interface rather than terminal keystroke injection. It
  adds lightweight tmux/cwd/git context, verifies that the target process
  starts, focuses it only when the source pane is still active, and closes the
  source after a cancellable 15-second grace period. A launch failure rolls
  back and leaves the source untouched. Use `tmux-agent-workbench agent profiles [--json]` to
  list targets and `tmux-agent-workbench agent handoff cancel` during the grace period. This works in
  any tmux session, not only sessions marked as task workbenches. If an agent's
  tool runner strips `TMUX`/`TMUX_PANE`, the command safely recovers its source
  pane from the process ancestry instead of guessing from the active client.

### Handoff profiles

Built-in profiles launch the user's default Codex, Claude Code, TraeCode CLI,
or opencode setup. Add or override profiles with `.conf` files under
`$XDG_CONFIG_HOME/tmux-agent-workbench/profiles/`:

```ini
# ~/.config/tmux-agent-workbench/profiles/codex-deep.conf
adapter=codex
description=Codex with high reasoning effort
model=gpt-5.6
effort=high
permissions=auto
env.EXAMPLE_FIXED_SETTING=value
```

Supported adapters are `codex`, `claude`, `trae`, `opencode`, and `command`.
The first four accept `model`, `effort` where the CLI supports it,
`permissions=auto` or `permissions=bypass`, and fixed `env.NAME=value` entries.
Automatic mode uses each CLI's approval reviewer/workspace-safe preset; bypass
remains available only when explicitly requested. A trusted user-level
custom launcher can use `adapter=command` plus `command=/absolute/path`; it is
called with the generated prompt file as its sole argument. Repository-local
profiles are deliberately not loaded.

The installer also exposes the bundled `handoff` skill through the shared
`~/.agents/skills` discovery directory. It teaches a source agent to summarize
the whole active task, select an exact profile when the user did not, and make
`tmux-agent-workbench agent handoff` its final action.

The bundled `reorganise-agents` skill groups existing agent windows into
sessions by task. It resolves `agent-team`'s Codex `batch` worker model and
effort and delegates the operation to one native subagent with fresh context.
It preserves the caller's execution permissions; it does not launch a second
interactive CLI or use the scene's lead model. Group names follow the current
tasks, and retaining a scratchpad session such as `0` is optional.

`tmux-agent-workbench reorganise batch-worker` prints the resolved worker
settings. The `snapshot`, `apply --snapshot FILE --plan FILE`, and
`restore RUN_DIR` subcommands provide the deterministic layout operations.
Migration records live under `$XDG_STATE_HOME/tmux-agent-workbench/reorganise`.
The worker instructions and plan format are maintained in the
[skill](skills/reorganise-agents/SKILL.md).

### Layer 2 — isolated task worktrees (`git/bin/`)

- **`new [--detach] [--task <name>] <direction> [repo[:branch] ...]`** creates or
  reopens a workspace session and its first task. The initial task defaults to
  the direction name. Repositories are optional. `new --prompt` opens the
  workspace-name prompt.
- **`task new [--detach] <name> [repo[:branch] ...]`** creates or reopens an
  independent task in the current workspace. It starts with one agent pane.
  `task new --prompt` opens a task-name popup; repositories can be added later.
- **`task list`** lists task names, owning windows (or `closed`), and paths.
- **`add [--base <remote-branch>] <repo>[:<branch>]`** creates a checkout at
  `<workspace>/tasks/<task>/<repo>`. Branches default to `task/<direction>/<task>`;
  an explicit `repo:branch` overrides that name. It fetches origin and creates
  missing branches from the requested remote base, or refreshed `origin/HEAD`.
  A checked-out branch cannot be reused by another task. Existing checkouts
  must belong to the requested repo and branch. Adding a repo opens no tool
  and does not change the selected window.
- Repositories can supply an executable `<main-checkout>/.workbench/worktree-init`.
  It runs from the task checkout on each `add`, with `WORKBENCH_INIT_PROTOCOL=1`,
  `WORKBENCH_MAIN_CHECKOUT`, `WORKBENCH_WORKTREE`, `WORKBENCH_WORKSPACE`,
  `WORKBENCH_FEATURE`, `WORKBENCH_TASK`, `WORKBENCH_TASK_ROOT`, `WORKBENCH_REPO`,
  and `WORKBENCH_BRANCH`. Symlink initializers are refused; a failed initializer
  retains the checkout for repair.
- **`task done [--force] <name>`** removes that task's checkouts and window.
  Uncommitted changes or scratch files retain the task. **`done [--force]
  <direction>`** closes the whole workspace, retaining unfinished tasks.
  `--force` discards worktree changes only; main checkouts and unrelated files
  are preserved. Closing the last task window may end the tmux session; the
  direction can be reopened later.
- **`projects rebuild [code_root] [workspace_root]`** generates tmuxinator configs
  from workspace/task directories. New workspaces restore one agent window per
  task. Existing workspace layouts reopen as one legacy task without moving
  their repos. Code-pool entries create an isolated first task instead of
  launching an agent in the shared source checkout. Temporary tools and service
  panes are not pre-created. Handmade configs and `.genignore` are preserved.
- **`promote` is retired.** Moving a running agent's pane cannot change its
  checkout or remembered paths. Use `task new` to start isolated work instead.

## Cold-start PATH caveat

Install the public command into a directory on your login shell's `PATH`
before creating the first workspace. From the plugin checkout, run:

```sh
./install ~/.local/bin        # or any dir on your PATH; --no-git skips layer 2
command -v tmux-agent-workbench
tmux-agent-workbench --help
```

Re-running is idempotent, and a real file already sitting at a target name is
backed up to `<name>~` rather than clobbered. Use
`tmux-agent-workbench <subcommand>` for shell and agent calls. Internal script
names and legacy compatibility aliases are not the public API.

## Agent conventions

_(For `AGENTS.md` / `CLAUDE.md` — how a coding agent uses this autonomously.)_

Workbench owns the agent conventions in
[`agent-conventions/AGENTS.snippet.md`](agent-conventions/AGENTS.snippet.md).
The installed public command prints that file without requiring a running tmux
server or daemon:

```sh
tmux-agent-workbench conventions
```

Keep only this reference in your global `AGENTS.md` / `CLAUDE.md`:

> Before operating tmux windows, agents, workspaces, or persistent dev servers,
> run `tmux-agent-workbench conventions` and follow its instructions.

The installer exposes the command but does not edit your agent instruction files.
The conventions cover stable task names, one agent per ordinary window,
workspace membership, and dev-server task panes. Updating Workbench updates the
rules without copying them into each tool's configuration.

### Stable task names

When a hook supplies a thread-name, Workbench uses that same name for the agent
card and the owned tmux window. Renaming a thread updates both. Manually renamed
windows remain protected. Task directories and IDs remain stable when labels
change. Codex task progress is read from its configured terminal-title item;
without an explicit progress value the sidebar omits it.

The plugin defaults `automatic-rename` to `off`. Once a task is clear, the agent
assigns a short semantic name through Workbench:

```sh
tmux-agent-workbench title set 'Repair cursor rendering'
tmux-agent-workbench title status
```

The command resolves the calling agent through process ancestry and verifies
the resulting name in tmux. If ancestry is hidden, use the verified `--pane`
and `--session-id` supplied by the lifecycle hook. It never follows the focused
client or guesses from a working directory. A live Workbench daemon is needed
to verify the process/session binding.

When a tool runner drops `XDG_RUNTIME_DIR`, Workbench recovers the runtime from
the tmux server before opening or starting its daemon. If older daemons remain
in other runtime directories, identical verified caller records count once;
different servers, sessions and process fingerprints remain distinct.

A tool sandbox can deny local Unix socket access even with the correct pane and
session. The command reports that permission error separately from an identity
mismatch. Choose execution permissions before the first invocation, using the
session's current sandbox and approval policy. If it blocks local sockets and
approvals are allowed, request approved execution of the exact title command
immediately. Do not use a failed naming attempt as a permission probe. If sockets
are already allowed, run normally; if approval is forbidden or unavailable while
sockets are blocked, skip naming and continue the task. Recreating a pane does
not grant socket access, and naming does not require changing the session's
sandbox or network policy.

Codex and Claude lifecycle hooks remind the model only while an ordinary
window has a default shell/process name. `SessionStart` and `UserPromptSubmit`
provide short context; `Stop` requests at most one naming continuation after
a submitted task. The continuation budget is stored with the tmux window, so
resume, compaction, duplicate hooks, and daemon restarts cannot reset it.
Issued naming bindings remain attached to the CLI process when auxiliary
sessions report hooks from that same process. The visible Codex thread identity
can verify an issued binding even if the detector's latest hook session changed;
an auxiliary session cannot take over naming or reset the repair budget.
If identity, tmux, or the naming lock is unavailable, the hook lets work proceed.
This improves execution reliability; the model still chooses the semantic name.

The first live interactive agent owns a shared window's name. Other agents,
sidebars, status updates and focus changes cannot replace it. Existing
meaningful names, manual edits, managed task windows, and legacy repository
inspection windows are preserved. The helper deliberately does not replace
an existing task name; an explicitly requested repurpose can use tmux's normal
rename command. Prefer one task and one agent per ordinary window; explicitly
requested team layouts remain intact.

When the naming owner's CLI exits, the daemon restores the pre-rename name and
the window's previous `automatic-rename` override (including inheritance), then
unsets the task-title ownership record. A completed response (`Stop`) is not an
exit. Cleanup checks the real process identity, so it also handles forced
termination, a closed owner pane with a surviving window, and exits that happened
while the daemon was offline. A verified same-task resume in the same pane keeps
ownership until its final exit. Manual title and automatic-naming changes made
after the task rename are preserved; other agents cannot release the owner's
title. Older records without saved option metadata can still restore their
original name.

To opt into tmux's automatic names, set this before loading the plugin:

```tmux
set -g @workbench-window-automatic-rename on
```

### Service panes and legacy cleanup

`run --name dev <own-checkout> -- npm run dev` uses `dev` as a service slot in
that task's window. Repeating it replaces the service in the same pane;
other tasks' services are independent. Stop it with `tmux kill-pane -t <id>`
when finished. The pane also closes automatically when the process exits.

The `prune` command remains available to clean obsolete inspection windows
from older installations. `prune --dead-tasks` also removes retained dead
service panes from those installations. It does not delete task worktrees or
unmarked windows. New workflows do not create inspection windows.

### Botmux (optional)

Install [botmux](https://github.com/deepcoldy/botmux) and Python 3 to enable
**Botmux…** in the Agent status menu. **Launch agent…** is available only in
the tmux status menu. Configuration, daemon
start/status, dashboard URL and connection listing run in an interactive popup.
Credentials and runtime state remain owned by botmux, outside this repository.

Choose **Prepare this pane’s connect command…**, then paste the copied `/adopt
session:window.pane` command into a fresh Feishu topic. Configure the bot for the
same coding agent and disable its sandbox. Clipboard forwarding depends on the
terminal; the popup also displays the command and tmux retains its paste buffer.
Copying is only preparation: the Feishu bot confirms the actual connection.
Botmux currently exposes adoption through its chat command, not a public local
CLI command. The menu does not read private botmux databases or claim live
per-pane binding status.

Use one topic per pane for parallel tasks; normally connect a team's lead.
Workers can be connected deliberately, and their menu offers a shortcut back
to the lead. Send `/close` in the corresponding topic to disconnect while
preserving the local agent. No global daemon stop is offered in the menu.
Botmux scans the default tmux server; custom sockets are rejected explicitly.
Pane identity and agent ancestry are checked before preparing the command.
