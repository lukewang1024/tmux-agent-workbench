# tmux-agent-workbench agent conventions

Workbench owns these conventions. Read them with `tmux-agent-workbench conventions`
before operating tmux windows, agents, workspaces, or persistent dev servers.
Global agent instructions need only point to that command; keep the rules here.

## Reorganising existing agents

For a request to classify agents and move their windows into sessions, use the
bundled `reorganise-agents` skill. It delegates the operation to one native
subagent using the configured `batch` worker model and effort. The skill owns
classification guidance; `tmux-agent-workbench reorganise` owns snapshots,
validated whole-window moves, verification, and layout restoration. Preserve
the caller's execution permissions and pass along any scratchpad preference.

## Stable task names for ordinary tmux agent windows

The agent thread-name is the shared topic for the sidebar and tmux window.
When available, Workbench synchronizes it automatically; do not invent a second
name. Task IDs, directories, and repository ownership remain fixed when a thread
is renamed. Without thread-name metadata, the fallback naming rules below apply.

Prefer one task and one agent per window. Do not start independent agents in
multiple panes of the same window unless the user explicitly requests it or an
opted-in team protocol requires it. Sidebars and supporting task panes are fine.
Do not rearrange existing panes or agents merely to enforce this preference.

For an ordinary agent window, once the task is clear, assign a short, meaningful
window name from the task (roughly 2–4 words or 6–12 Chinese characters) using
`tmux-agent-workbench title set '<task-name>'`. Workbench resolves the calling
agent through process ancestry. When a tool sandbox hides ancestry, use the
verified `--pane` and `--session-id` supplied by the lifecycle hook; never use
the human's currently selected window. `title status` explains the owner,
current title, and whether the one-time naming repair has been used.
Choose execution permissions **before the first invocation**, using the current
session's sandbox and approval policy. The command needs local Unix-socket access.
When the sandbox blocks these sockets and approval is allowed, request approved
execution of this exact title command immediately; do not first run a restricted
attempt as a permission probe. In Codex `exec_command`, use
`sandbox_permissions="require_escalated"` with a naming-specific justification
only when the current tool schema and approval policy allow it. When socket access
is already permitted, run normally. When socket access is blocked and approval is
forbidden or unavailable, skip naming and continue the task. Keep the current pane
and its sandbox/network policy; opening another pane does not grant socket access.
Keep that name stable through
status, title, command, and pane-focus changes. Do not derive it from pane titles,
process names, model names, or session IDs. Preserve an existing meaningful name
unless the user asks to rename it or clearly repurposes the window for a new task.
Workbench-managed agent/repository/task windows retain their prescribed names.

For supported lifecycle hooks, an unnamed ordinary window gets a short naming
reminder on session start and prompt submission. If it is still unnamed when
the first task ends, Workbench requests one naming continuation. This repair
budget survives resume, compaction, and daemon restarts. Missing identity or
unavailable tmux must not prevent the actual task from proceeding. In a shared
window, the first live interactive agent owns its title; other agents preserve
it. Existing meaningful names and manual changes always take precedence.
When the naming owner's CLI actually exits, the daemon restores the previous
window name and automatic-naming options, then clears its ownership marker.
A finished response is not an exit. Manual edits are preserved, and a verified
same-task resume in the same pane keeps ownership until its final exit. Agents
do not need to undo their title in a Stop hook or on every completed response.
Clearing a Codex session starts a new task: Workbench restores the previous
window name and automatic-naming options before naming the new task.

## Workspace sessions and task windows

A workspace session groups a direction or larger goal. Each task has one window,
one interactive agent, and its own directory. Panes in that window support the
same task; do not put independent tasks or agents in additional panes.

Use the public `tmux-agent-workbench` command for workbench operations. A new
workspace starts with one task; repositories are optional:

```sh
tmux-agent-workbench new --task formulas word-experience bear-web-x
tmux-agent-workbench task new image-layout bear-web-x
tmux-agent-workbench task list
```

The two tasks above have separate checkouts of the same repository under
`$WORKBENCH_WORKSPACE_ROOT/word-experience/tasks/<task>/bear-web-x`.
Each defaults to a separate `task/word-experience/<task>` branch. One task may own
multiple repositories. Independent tasks must not share a working checkout,
index, or branch. Shared machine/environment maintenance is an exceptional
scope: make that choice explicit instead of silently reusing another task's
checkout. Reading a main checkout to understand code does not enroll it as a
writable task checkout.

When a task needs another repository, run `tmux-agent-workbench add <repo>` from
its window. This creates and initializes an isolated worktree in that task;
it does not create a window, launch tools, or change focus. Never reuse another
task's worktree because it belongs to the same Git repository. Give the agent
access to its own task directory using that agent's normal scope mechanism.

In ordinary, ad-hoc sessions, do not create workbench windows implicitly. If
code changes require isolation, create a dedicated Git worktree without changing
the user's tmux layout, or use an explicitly requested workspace/task workflow.

`add`, `run`, and temporary tools resolve the calling pane and its owning task,
not whichever client window is selected. When a tool environment or PID sandbox
hides caller identity, provide the verified `TMUX_PANE`, or set both
`WORKBENCH_SESSION` and `WORKBENCH_TASK` explicitly. A session alone is not enough
when multiple tasks exist. `WORKBENCH_FEATURE` remains a legacy session selector.
Connection failures do not imply an ordinary session or authorize guessing a
different task.

## Tools, commands, and task cleanup

Task windows begin with their agent. There is no inspection layout and no
mandatory Git, editor, scratchpad, or dev-server pane. Open tools only when
needed:

```sh
tmux-agent-workbench inspect <own-checkout>             # temporary Git popup
tmux-agent-workbench inspect <own-checkout> --editor    # temporary editor popup
tmux-agent-workbench inspect <own-checkout> --shell --pane
```

Popups and tool panes close when their command exits. Close any task-owned
resources once their use is finished; preserve the user's pre-existing panes.
The repo picker lists the owning task's repositories when run in a task window.

Run one-shot builds, tests, lint, installs, and other commands through the
agent's execution tools, including long one-shot work. Only persistent dev
servers in a workspace use a supporting pane:

```sh
tmux-agent-workbench run --name dev <own-checkout> -- npm run dev
```

`run` creates or reuses a named pane within the calling task's window. The same
name in another task is independent. It prints the pane ID; use that ID to read
output or stop the service with `tmux kill-pane` when finished. It never opens
an inspection window or routes to a sibling task by Git common-directory
identity. Service panes close when their commands exit. In ordinary sessions,
use the agent's background-task mechanism without changing the user's layout.

`task done <name>` removes only that task's worktrees and window. It retains a
task with uncommitted changes or scratch files. `done <direction>` closes the
whole workspace, retaining unfinished tasks. `--force` discards changes only in
worktrees; it does not delete unrelated files or a main checkout. Existing
workspaces remain a single legacy task and are not moved automatically. New
parallel tasks always get independent worktrees. Rebuilding project configs
restores task windows, without restoring temporary tools.

Do not move a running agent into a newly created checkout: its current directory
and remembered paths still refer to the old checkout. The legacy `promote`
command is retired; start an isolated task with `task new` instead.
