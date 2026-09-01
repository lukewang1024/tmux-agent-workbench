# tmux-agent-workbench

tmux plugin for running agentic coding tasks (Claude Code, Codex, opencode)
inside tmux, where they actually live for hours or days instead of a single
terminal command.

Workbench v2 also discovers Codex, Claude, Trae, and OpenCode processes in
every pane of the current tmux server and maintains one canonical attention
snapshot. A per-server Rust daemon classifies `working`, `blocked`, `idle`, and
`unknown`, derives unseen `done`/blocked attention, drives a responsive sidebar
and fzf pickers, and owns focus-aware desktop/sound notification delivery. It
does not require agents to be launched by Workbench and it never displays or
persists prompt text by default. See the durable
[v2 specification](docs/attention-v2.md) and
[implementation status](docs/attention-v2-status.md).

## What it is / why

An agentic coding task rarely stays inside one repo. It starts in `web-app`,
then needs to check `shared-schemas`, then needs a throwaway branch of `api-service` to test
against — and the human running it needs a durable place to *watch* all of
that happen, not just a scrollback of agent output. A plain terminal tab
doesn't hold that shape. tmux does, if something wires up the convention:

- **one tmux session = one task**, with one window dedicated to the agent
  driving it;
- every other repo the task touches gets its own **inspection window** (git
  TUI + scratchpad + editor), so a human can look at what the agent is doing
  to that repo without leaving tmux;
- optionally, every repo folded into a task is its own **git worktree** on
  its own branch, so a multi-repo task never touches your main checkouts.

This repo is two independently loadable layers that implement that:

- **Layer 1** (`workbench.tmux` + `bin/`) — repo-agnostic. Just the
  session/window model and the inspection-window mechanic. No opinion about
  git worktrees, no opinion about where your repos live.
- **Layer 2** (`git/workbench-git.tmux` + `git/bin/`) — opinionated. Adds
  git-worktree-backed multi-repo workspaces on top of layer 1, assuming a
  `~/Code` (persistent checkouts) + `~/Workspace` (per-task worktrees)
  convention. Opt out of this layer entirely with `@workbench-disable-git 1`
  if you only want layer 1.

Both layers live in one repo and load from a single `@tpm_plugins` entry —
see [Install](#install).

## Requirements

- **tmux >= 3.2**, **git**, and the shipped `tmux-agent-workbench` binary —
  hard requirements. The installer downloads a matching macOS/Linux
  x86_64/aarch64 release, then falls back to a local Cargo build.
- **tig** or **lazygit** (or any git TUI) — launched in the top pane of every
  inspection window. Defaults to `tig`; set `@workbench-git-tool` to change
  it. Soft dependency: if the configured tool isn't installed, that one pane
  just fails to launch, nothing else breaks.
- **nvim** or **vim** — launched in the bottom pane of every inspection
  window. Uses `$VISUAL`/`$EDITOR` if set, else `nvim`, else `vim`.
- **fzf-tmux** — hard requirement for the inspect, session, and Agent pickers.
  Other daemon/sidebar behavior remains available if it is missing, and
  `tmux-agent-workbench doctor` reports the dependency clearly.
- **zoxide** — soft dependency of the picker: widens its candidate list with
  your directory-jump history. Its absence degrades the picker (fewer
  candidates), it doesn't break it.
- **tmuxinator** and **sesh** — soft dependency of the *persistent pool*
  layer 2 generates. `gen-tmuxinator-configs` only ever writes YAML under
  `~/.config/tmuxinator/`; it never shells out to the `tmuxinator` binary
  itself. Whether that YAML actually gets used to spin up sessions later is
  on you (`tmuxinator start <name>`, or `sesh connect` if it reads the same
  pool) — install them if you want that part of the workflow, skip them if
  you're happy driving `ws-new`/`ws-add` directly.

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
Agent configuration.

If you want layer 2 disabled from the start, set this **before** the
`@tpm_plugins` line so it's in effect the first time the plugin loads:

```tmux
set -g @workbench-disable-git 1
```

## Keybindings

Workbench uses the sidebar as its control surface. By default the only global
binding is `prefix+Tab`; once focused, press `?` for a centered list of plain
single-key actions. Optional global bindings are (re)installed each time the
plugin loads when their option is explicitly set.

| Action | Option | Default | Runs |
|---|---|---|---|
| Pick/start a tmuxinator project | `@workbench-key-project` | unbound | `workbench-session-pick` |
| Open the inspect picker (fzf) | `@workbench-key-inspect` | unbound | `mux-inspect-pick` |
| Regenerate the tmuxinator pool | `@workbench-key-regen` | unbound | `gen-tmuxinator-configs` |
| Prompt for a new workspace | `@workbench-key-new` | unbound | `ws-new-prompt` |
| Promote current pane's repo to a new task | `@workbench-key-promote` | unbound | `ws-promote` |
| Toggle current-window Agent sidebar | `@workbench-key-sidebar` | `Tab` | Rust sidebar |
| Toggle all Agent sidebars | `@workbench-key-sidebar-all` | unbound | Rust sidebar |
| Pick any session by attention | `@workbench-key-session` | unbound | session picker |
| Pick any detected Agent | `@workbench-key-agent` | unbound | Agent picker |
| Jump to next unseen attention | `@workbench-key-attention` | unbound | attention queue |

The Agent sidebar/session/Agent/attention actions belong to layer 1 and do not
depend on the git-worktree layer. Regenerate/new/promote require layer 2. Their
global options remain available for users who prefer direct prefix shortcuts;
the default avoids claiming memorable tmux keys for infrequent operations.

**Check for collisions before trusting any default, including these ones** —
a key that's free in your `tmux.conf` text can still be silently claimed by
a plugin loaded later via TPM (this exact thing happened choosing these
defaults: plain `g` looked free in the config file but `tmux-sessionist`
grabs it at load time). Verify with the *live*, post-plugin-load table, not
just the file:

```sh
tmux list-keys -T prefix | grep -E '(^| )(G|M-g|M-t|M-T)( |$)'
```

To rebind, set the option **before** the plugin loads (add it above
`@tpm_plugins` in `tmux.conf`, or set it and reload the plugin), e.g.:

```tmux
set -g @workbench-key-inspect 'g'
set -g @workbench-key-regen 'M-r'
set -g @workbench-key-new 'n'
set -g @workbench-key-promote 'M-p'
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
| `@workbench-agent` | `WORKBENCH_AGENT` | `claude` | Which CLI `mux-agent` execs: `claude`, `codex`, `trae`, or `opencode`. |
| `@workbench-git-tool` | `WORKBENCH_GIT_TOOL` | `tig` | Git TUI launched in the top pane of every inspection window (e.g. `lazygit`). |
| `@workbench-code-root` | `WORKBENCH_CODE_ROOT` | `~/Code` | Root of your persistent repo checkouts. Layer 2 looks here to resolve a short repo name (`ws-add web-app`) to its main checkout. |
| `@workbench-workspace-root` | `WORKBENCH_WORKSPACE_ROOT` | `~/Workspace` | Root under which `ws-new` creates one directory per workspace; each level-1 directory is pickable, with immediate child git worktrees as inspection windows (or the directory itself for a standalone checkout). |
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

The layout controller also honors the established tmux options
`@sidebar_width`, `@sidebar_position`, `@sidebar_auto_create`,
`@sidebar_min_width`, `@sidebar_max_width`, and
`@sidebar_main_min_width`. Explicit tmux values win over seeded config values.
All attention bindings can be changed with `@workbench-key-sidebar`,
`@workbench-key-sidebar-all`, `@workbench-key-session`,
`@workbench-key-agent`, and `@workbench-key-attention` before the plugin loads.

The sidebar shows all sessions and all detected agents from the current tmux
server in separate sections. Sessions occupy the upper half by default. A
midpoint action row keeps `new` on the left and `menu` on the right, with Agents
below it in the lower half. When the available terminal height cannot show all
rows, the order adapts to Agents, the action row, then Sessions so live Agent
state remains visible on short terminals such as phone clients. One blank row
separates the action row from both sections. Help remains available from `?`
without taking space in the mouse action row. Session and Agent targets use
two-line cards so the full card is easier to click. The label at the right of
the Agents header toggles between `grouped`
(stable session/window/pane order) and `prioritized` (blocked, unseen done,
working, seen idle, unknown); the choice is remembered and synchronized across
all running sidebar instances. Press `d` to toggle a more detailed view with
Agent kind, human-readable window/pane indices, process id, state source, hook
health, and matched rule. Enter or left-click focuses a row; `m` or right-click
opens session/agent actions. The footer opens the tmuxinator-project picker and
the global action menu. In responsive popup mode, `Escape`, `Ctrl-C`, and
`Ctrl-D` close the popup; the footer and global menu also expose a mouse-driven
close action. Those exit keys are popup-only and do not terminate a persistent
sidebar pane. Activating a session or Agent with Enter or a left click also
closes the popup after the navigation succeeds.

With focus in the sidebar, press `?` for the complete centered shortcut card.
The everyday keys are plain letters: `i` inspects a repo, `d` toggles details,
`n` jumps to attention, and `s`/`a` open pickers. Heavier mutating actions use
uppercase: `N` picks/starts a tmuxinator project, `W` creates a workspace, `P`
promotes the selected repo, and `R` rebuilds generated projects. `m` opens the
row menu. The global menu exposes the
same management actions for mouse use.

The daemon publishes `@workbench_window_state` and
`@workbench_window_label` as window-scoped tmux options for status themes.
They aggregate Agents in that window using `blocked > working > unseen done >
idle`; both options are unset when the window has no Agent. This keeps process
detection in Workbench while allowing a theme to render a lightweight current-
window badge without polling or screen scraping.

Per-Agent manifest overrides live at
`$XDG_CONFIG_HOME/tmux-agent-workbench/manifests/{codex,claude,trae,opencode}.toml`.
One local file replaces that Agent's whole bundled manifest; Workbench does not
download rules. See [the accepted v2 specification](docs/attention-v2.md) for
the version-1 matcher and state semantics.

### Attention commands

```sh
tmux-agent-workbench daemon ensure|status|stop
tmux-agent-workbench snapshot --json
tmux-agent-workbench agent explain %3 [--show-content]
tmux-agent-workbench metadata report --pane %3 --label build --ttl-ms 5000
tmux-agent-workbench config check
tmux-agent-workbench reload
tmux-agent-workbench doctor
```

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

Workbench also consumes two non-lifecycle CESP signals without changing the
canonical pane state: `session.start` plays a greeting once for startup/resume
(not compaction), while native failure events and failed `PostToolUse` payloads
play `task.error` and optionally show a background error notification. Raw tool
payloads are inspected only inside the short-lived reporter and are never sent
to the daemon or persisted. OpenPeon categories are resolved from the active
pack, so a separate notification hook such as Peon Ping is not required.

### SSH notification relay

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
and execute only Workbench's validated fixed focus command.

### Migration from tmux-agent-sidebar

Remove the old `tmux-agent-sidebar` TPM item, hook installer/updater calls,
hooks containing its path, and obsolete rich-UI options before enabling v2.
Do not load both plugins: Workbench refuses to create a second sidebar, and
`tmux-agent-workbench doctor` prints the remaining cleanup steps. V2 does not
modify Claude, Codex, Trae, or OpenCode hook/config files; optional metadata is
reported through the TTL-bound command above.

## Commands

### Layer 1 — repo-agnostic (`bin/`)

- **`mux-agent`** — launches the task's coding agent (`$WORKBENCH_AGENT`:
  `claude` / `codex` / `trae` / `opencode`) in the current directory, and stamps the
  session with `@workbench_task 1`. It also stamps the agent pane with
  `@workbench_agent` and `@workbench_profile`, allowing handoffs to recover
  launch identity even when an agent tool runner sanitizes child-process
  environment variables. The session marker is the single flag that
  opts a session into the window-per-role model — a bare `claude` started
  in some other session stays unmarked and `mux-inspect` no-ops there. Codex
  task workbenches always launch in YOLO mode so routine approvals do not
  interrupt the task driver; an explicitly supplied equivalent flag is kept
  without adding a duplicate.
- **`mux-inspect <repo-path> [--focus] [--force]`** — adds (or focuses) a
  repo as an inspection window in the current (or `$WORKBENCH_SESSION`)
  task session: three panes, even-vertical, cwd = the repo in all of them —
  git tool on top, empty scratchpad shell in the middle (lands here),
  editor on the bottom. Idempotent (re-running just focuses the existing
  window). Self-gates on `@workbench_task` unless `--force` is given — the
  agent can call it unconditionally without checking session type first.
  This is also the command the coding agent itself is meant to call, per the
  [agent conventions](#agent-conventions) below, the moment its work reaches
  into another repo. Workbench v2's own responsive sidebar is added after the
  inspection layout is built and stays outside workspace-pane layout changes.
- **`mux-inspect-pick`** (prefix+G) — fzf-tmux picker over your directory
  universe (zoxide history + the generated tmuxinator pool + a live `fd`
  search), then opens the pick as a focused inspection window via
  `mux-inspect --focus --force`. The manual counterpart to the agent-driven
  `mux-inspect` call.
- **`tmux-agent-workbench run [--name <label>] <repo-path> -- <argv...>`** — appends a
  detached pane to that repo's inspection window for a dev server or another
  long-running task. It creates the inspection window first when necessary,
  tags the new pane with `@pane_role=task`, `@workbench_task_name`,
  `@workbench_task_command`, and `@workbench_project_root`, starts the command,
  then reapplies `even-vertical`. When `tmux-layout-keep-sidebar` is available,
  the sidebar remains a full-height side column and only workspace panes are
  re-laid out. The pane id is printed so callers can capture logs or remove it
  later. Long tasks are added on demand; the default inspection layout remains
  git/shell/editor only.
  The legacy `mux-run-task` form remains as an every-invocation-warning shim
  through 2.1. Use `--shell '<command>'` only when shell syntax is required.
- **`mux-handoff --target <profile> < summary`** — hands the current task to a
  fresh coding-agent profile in a detached right-hand pane, using the target
  CLI's initial-prompt interface rather than terminal keystroke injection. It
  adds lightweight tmux/cwd/git context, verifies that the target process
  starts, focuses it only when the source pane is still active, and closes the
  source after a cancellable 15-second grace period. A launch failure rolls
  back and leaves the source untouched. Use `mux-handoff profiles [--json]` to
  list targets and `mux-handoff cancel` during the grace period. This works in
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
permissions=bypass
env.EXAMPLE_FIXED_SETTING=value
```

Supported adapters are `codex`, `claude`, `trae`, `opencode`, and `command`.
The first four accept `model`, `effort` where the CLI supports it,
`permissions=bypass`, and fixed `env.NAME=value` entries. A trusted user-level
custom launcher can use `adapter=command` plus `command=/absolute/path`; it is
called with the generated prompt file as its sole argument. Repository-local
profiles are deliberately not loaded.

The installer also exposes the bundled `handoff` skill through the shared
`~/.agents/skills` discovery directory. It teaches a source agent to summarize
the whole active task, select an exact profile when the user did not, and make
`mux-handoff` its final action.

### Layer 2 — opinionated git-worktree workspaces (`git/bin/`)

- **`ws-new <feature> [repo[:branch] ...]`** — starts a new task: creates
  `$WORKBENCH_WORKSPACE_ROOT/<feature>`, a same-named tmux session marked
  as a task workbench, and its agent window. Repos are optional — a
  workspace can start empty ("what does this task even touch?") and grow
  later. Any repos given up front are handed to `ws-add` one at a time.
  Idempotent: re-running against an existing workspace just folds in more
  repos (or re-attaches).
- **`ws-new-prompt`** (prefix+M-t) — `tmux command-prompt` front end for
  `ws-new`: type `feature` alone, or `feature repo[:branch] ...`, in one go.
- **`ws-add <repo>[:<branch>]`** — folds a repo into an *already-running*
  workspace: creates its worktree under
  `$WORKBENCH_WORKSPACE_ROOT/<feature>/<repo>` if it doesn't exist yet
  (branch defaults to the workspace's own name; base ref is the repo's
  `origin/HEAD` if resolvable, else current HEAD), then adds it as an
  inspection window via `mux-inspect`. This is the "discovered mid-task"
  primitive — call it the moment a task turns out to need a repo that
  wasn't known about at `ws-new` time. Usable by a human at the prompt or
  by a coding agent's own shell tool. Idempotent: an existing worktree is
  initialized again and focused, not recreated.

  A repository can opt into local worktree initialization by providing an
  executable regular file at `<main-checkout>/.workbench/worktree-init`.
  `ws-add` runs this trusted main-checkout script after the worktree exists and
  before adding its inspection window, on every invocation. The script runs
  with the worktree as its current directory and receives
  `WORKBENCH_INIT_PROTOCOL=1`, `WORKBENCH_MAIN_CHECKOUT`,
  `WORKBENCH_WORKTREE`, `WORKBENCH_WORKSPACE`, `WORKBENCH_FEATURE`,
  `WORKBENCH_REPO`, and `WORKBENCH_BRANCH`. It must be non-interactive and
  idempotent. A non-zero exit stops `ws-add` while preserving the worktree for
  a later repair; missing initializers are a no-op. Symlink initializers are
  refused.
- **`ws-done [--force] <feature>`** — tears a workspace down: kills its
  tmux session if running, then `git worktree remove` per member repo
  (each repo's main checkout under `$WORKBENCH_CODE_ROOT` is never
  touched). Default policy is safe-clean: a dirty worktree is left in
  place and reported rather than discarded; `--force` discards it anyway
  (`git worktree remove --force`). Regenerates the tmuxinator pool
  afterward so the stale config disappears with it.
- **`ws-promote [feature]`** (prefix+M-T) — spins whatever the *current
  pane* is doing into its own brand-new dedicated task. Unlike `ws-new`, the
  pane itself moves there (`tmux break-pane`) rather than a disconnected
  blank pane appearing elsewhere — a coding agent already mid-task in that
  pane keeps running, uninterrupted, now living in the new session's `agent`
  window. A repo isn't required — a task can start from anywhere: if the
  pane is inside one, that repo is folded in (new worktree and branch;
  `feature` defaults to its short name); if it isn't, the task still starts
  with no repo attached (grow it later with `ws-add`), and if you didn't
  pass `feature` either, this *asks* for a name rather than guessing one off
  the bare directory name. Refuses cleanly (leaving the source pane/session
  untouched) if `feature` is already a session name. The *source* session is
  expected to disappear once its only pane moves out — that's normal tmux
  behavior, not a bug, and is the point: you don't end up straddling two
  places. If tmux-agent-sidebar is installed with auto-create on, the new
  `agent` window gets one explicitly here — `tmux break-pane` doesn't fire
  the `after-new-window` hook that every other window in this system relies
  on for that, so without this it would be the one place that never got a
  sidebar automatically.
- **`gen-tmuxinator-configs [code_root] [workspace_root]`** (prefix+M-g) —
  regenerates the persistent tmuxinator pool under
  `~/.config/tmuxinator/` from actual git-worktree state: one config per
  `$WORKBENCH_CODE_ROOT` repo/pool-slot, and one config for every level-1
  `$WORKBENCH_WORKSPACE_ROOT` directory. Workspace configs are pre-seeded
  with an inspection window for each immediate child git worktree, or for the
  directory itself when it is a standalone checkout; empty task directories
  get an agent-only config. Positional args win over
  `$WORKBENCH_CODE_ROOT`/`$WORKBENCH_WORKSPACE_ROOT`, which win over the
  `~/Code` / `~/Workspace` hardcoded defaults. This script only ever writes
  YAML — it never invokes the `tmuxinator` binary itself (see
  [Requirements](#requirements)). `ws-add`, `ws-done`, and the prefix+M-g
  binding all call it so the pool stays in sync with reality; manual
  exclusions persist across re-runs via
  `~/.config/tmuxinator/.genignore`.

## Cold-start PATH caveat

Both `.tmux` entrypoints prepend their own `bin/` to the tmux **global**
environment `PATH` at load time. That covers every process tmux spawns
from then on — new panes, new windows, an agent's own shell-tool calls
invoking `mux-inspect` or `ws-add` by bare name — automatically.

It does **not** cover the shell you're typing into *before* any of that has
happened. Specifically: bootstrapping the very first workspace by running
`ws-new` from an ordinary login shell that has never started (or attached
to) a tmux server with this plugin loaded will fail with "command not
found," because that shell's `PATH` was never touched by tmux.

Fix it the ordinary way — put the plugin's bin dirs on your shell's own
`PATH` in `.bashrc`/`.zshrc` (adjust the clone path to wherever TPM put it):

```sh
export PATH="$HOME/.tmux/plugins/tmux-agent-workbench/bin:$HOME/.tmux/plugins/tmux-agent-workbench/git/bin:$PATH"
```

Once you're inside any session this plugin has touched, every subsequent
pane/window/task inherits the PATH tmux set — this is strictly a one-time,
outside-of-tmux bootstrap concern.

If you'd rather have discrete commands on `PATH` (so shell completion and
`command -v ws-new` work) than a `PATH` entry, run the bundled installer
instead — it symlinks every command this plugin ships into a bin dir you
already have on `PATH`, and knows its own command list so you never enumerate
them:

```sh
./install ~/.local/bin        # or any dir on your PATH; --no-git skips layer 2
```

Re-running is idempotent, and a real file already sitting at a target name is
backed up to `<name>~` rather than clobbered.

## Agent conventions

_(For `AGENTS.md` / `CLAUDE.md` — how a coding agent uses this autonomously.)_

For a coding agent to use any of this autonomously — folding a repo it just
started touching into the current inspection windows, or calling `ws-add`
when a task turns out to need another repo — it has to be told the
convention exists. A tmux plugin has no reach into `~/.claude`, `~/.codex`,
`~/.config/opencode`, or your repo's own `AGENTS.md`/`CLAUDE.md`; it cannot
install itself into an agent's instructions. That's a real limitation, not
an oversight — nothing here writes to those files for you.

[`agent-conventions/AGENTS.snippet.md`](agent-conventions/AGENTS.snippet.md)
is the text meant to close that gap. Copy its contents into your own
`AGENTS.md` / `CLAUDE.md` (repo-level or global, wherever your agent reads
project conventions from) so it knows to call `mux-inspect` / `ws-add` when
its work crosses into another repo, and how each of the three supported
agents (Claude Code, Codex, opencode) should separately grow its own
*write* scope once the window exists.
