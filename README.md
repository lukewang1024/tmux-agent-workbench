# tmux-agent-workbench

tmux plugin for running agentic coding tasks (Claude Code, Codex, opencode)
inside tmux, where they actually live for hours or days instead of a single
terminal command.

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

- **tmux** and **git** — hard requirements for everything.
- **tig** or **lazygit** (or any git TUI) — launched in the top pane of every
  inspection window. Defaults to `tig`; set `@workbench-git-tool` to change
  it. Soft dependency: if the configured tool isn't installed, that one pane
  just fails to launch, nothing else breaks.
- **nvim** or **vim** — launched in the bottom pane of every inspection
  window. Uses `$VISUAL`/`$EDITOR` if set, else `nvim`, else `vim`.
- **fzf-tmux** — hard requirement for the inspect picker (`mux-inspect-pick`,
  bound to prefix+G). Layer 1 checks for it at load time and posts a tmux
  message if it's missing; nothing else in layer 1 depends on it.
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

If you want layer 2 disabled from the start, set this **before** the
`@tpm_plugins` line so it's in effect the first time the plugin loads:

```tmux
set -g @workbench-disable-git 1
```

## Keybindings

All bindings are `prefix`-prefixed (not root-table), and are (re)installed
each time the plugin loads (fresh install, `prefix+I`, or a config reload).

| Action | Option | Default | Runs |
|---|---|---|---|
| Open the inspect picker (fzf) | `@workbench-key-inspect` | `G` | `mux-inspect-pick` |
| Regenerate the tmuxinator pool | `@workbench-key-regen` | `M-g` | `gen-tmuxinator-configs` |
| Prompt for a new workspace | `@workbench-key-new` | `M-t` | `ws-new-prompt` |
| Promote current pane's repo to a new task | `@workbench-key-promote` | `M-T` | `ws-promote` |

The last three only bind if layer 2 is enabled (`workbench-git.tmux`
actually loaded). `M-t`/`M-T` are a deliberate pair: the plain Meta key
starts a new workspace/session and asks what it needs, the Shift variant
derives one from the pane you're already in with no prompt — grouped under
`t` because plain `T` (elsewhere in a typical `tmux.conf`, e.g. via
[sesh](https://github.com/joshmedeski/sesh)) is already "pick an *existing*
session," so the family reads as one session-flavored cluster. `M-T` is
capital (Meta+Shift+t), distinct from `M-t`. `G`/`M-g` are a separate pair
(inspect / regenerate the pool) and don't need to relate to this one.

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
| `@workbench-agent` | `WORKBENCH_AGENT` | `claude` | Which CLI `mux-agent` execs: `claude`, `codex`, or `opencode`. |
| `@workbench-git-tool` | `WORKBENCH_GIT_TOOL` | `tig` | Git TUI launched in the top pane of every inspection window (e.g. `lazygit`). |
| `@workbench-code-root` | `WORKBENCH_CODE_ROOT` | `~/Code` | Root of your persistent repo checkouts. Layer 2 looks here to resolve a short repo name (`ws-add web-app`) to its main checkout. |
| `@workbench-workspace-root` | `WORKBENCH_WORKSPACE_ROOT` | `~/Workspace` | Root under which `ws-new` creates one directory per multi-repo workspace, each member a git worktree. |
| `@workbench-disable-git` | — (checked directly, not bridged) | unset | Set to `1` to skip loading layer 2 (`git/workbench-git.tmux`) entirely. |

```tmux
set -g @workbench-agent 'codex'
set -g @workbench-git-tool 'lazygit'
set -g @workbench-code-root '~/Code'
set -g @workbench-workspace-root '~/Workspace'
```

## Commands

### Layer 1 — repo-agnostic (`bin/`)

- **`mux-agent`** — launches the task's coding agent (`$WORKBENCH_AGENT`:
  `claude` / `codex` / `opencode`) in the current directory, and stamps the
  session with `@workbench_task 1`. That marker is the single flag that
  opts a session into the window-per-role model — a bare `claude` started
  in some other session stays unmarked and `mux-inspect` no-ops there.
- **`mux-inspect <repo-path> [--focus] [--force]`** — adds (or focuses) a
  repo as an inspection window in the current (or `$WORKBENCH_SESSION`)
  task session: three panes, even-vertical, cwd = the repo in all of them —
  git tool on top, empty scratchpad shell in the middle (lands here),
  editor on the bottom. Idempotent (re-running just focuses the existing
  window). Self-gates on `@workbench_task` unless `--force` is given — the
  agent can call it unconditionally without checking session type first.
  This is also the command the coding agent itself is meant to call, per
  the [agent conventions](#agent-conventions) below, the moment its work
  reaches into another repo. If [tmux-agent-sidebar](https://github.com/hiroppy/tmux-agent-sidebar)
  is installed with auto-create on, it adds its own pane here same as any
  other window — its full-height split narrows the 3-pane stack uniformly
  rather than disturbing it, so this window doesn't opt out.
- **`mux-inspect-pick`** (prefix+G) — fzf-tmux picker over your directory
  universe (zoxide history + the generated tmuxinator pool + a live `fd`
  search), then opens the pick as a focused inspection window via
  `mux-inspect --focus --force`. The manual counterpart to the agent-driven
  `mux-inspect` call.

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
  just focused, not recreated.
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
  `$WORKBENCH_CODE_ROOT` repo/pool-slot, one config per
  `$WORKBENCH_WORKSPACE_ROOT` workspace (pre-seeded with every member repo
  as its own inspection window). Positional args win over
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
