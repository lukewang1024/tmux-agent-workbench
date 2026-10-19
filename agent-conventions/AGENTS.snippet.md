# tmux-agent-workbench agent conventions

Workbench owns these conventions. Read them with `tmux-agent-workbench conventions`
before operating tmux windows, agents, workspaces, or persistent dev servers.
Global agent instructions need only point to that command; keep the rules here.

## Stable task names for ordinary tmux agent windows

Prefer one task and one agent per window. Do not start independent agents in
multiple panes of the same window unless the user explicitly requests it or an
opted-in team protocol requires it. Sidebars and supporting task panes are fine.
Do not rearrange existing panes or agents merely to enforce this preference.

For an ordinary agent window, once the task is clear, assign a short, meaningful
window name from the task (roughly 2–4 words or 6–12 Chinese characters) using
`tmux rename-window -t <verified-window-id> <task-name>`. Resolve the owning
window from the agent's verified pane/session identity; do not assume the human's
currently selected window belongs to this agent. Keep that name stable through
status, title, command, and pane-focus changes. Do not derive it from pane titles,
process names, model names, or session IDs. Preserve an existing meaningful name
unless the user asks to rename it or clearly repurposes the window for a new task.
Workbench-managed agent/repository/task windows retain their prescribed names.

## tmux task/window model — folding another repo into the task

Some tmux sessions are **task workbenches**: one session is one task, you (the
agent) are the driver living in the `agent` window, and each repo the task
touches gets its own inspection window (git/tig + shell). Such a session is
marked with `@workbench_task` by the workbench launcher. You may equally be
running in an **ordinary** session (a bare agent the human started ad-hoc);
there, this whole mechanism stays out of the way and you do nothing special
for cross-repo work.

Use the public `tmux-agent-workbench` command for all workbench operations.
Legacy command aliases are compatibility-only; do not use them in new calls.

When your work starts touching a **local repo other than the one you started in**:

1. First check whether you're in a **workspace** session — its root (and every
   repo already folded in) lives under `~/Workspace/<feature>/...` rather than
   directly under `~/Code`. That's a multi-repo task: each member repo is its
   own git worktree/branch dedicated to this feature, not just a read-only
   peek at the repo's shared `~/Code` checkout.
   - **Workspace session, repo not yet a member** — run
   `tmux-agent-workbench add <repo-short-name>[:<branch>]` (e.g. `tmux-agent-workbench add web-app`) instead of
   `tmux-agent-workbench inspect`. It creates that repo's worktree under this workspace
   (branch defaults to the workspace's own name) *and* folds it in as an
   inspection window in one step. Idempotent — re-running it once the
   worktree exists just re-focuses the window.
   - **Workspace session, repo already a member** — do not run `inspect`; its
     inspection window already exists.
   - **Ordinary session** (including a checkout from the `~/Code` pool) — do
     not run `inspect`, `add`, or any other workbench window command. Work on
     the repo without changing the user's tmux layout; do not use
     `inspect --force` as a workaround.
2. In the first case only, let `tmux-agent-workbench add` create the worktree
   and inspection window. If the workspace repo is already a member, there is
   no inspection command to run.
3. Bring the repo into your own **write** scope (the mechanism differs per agent):
   - **Claude Code** — `/add-dir <path>` (effective immediately).
   - **Codex** — you can already read it; to write there the session must be
     relaunched with `--add-dir <path>` (or add it to
     `sandbox_workspace_write.writable_roots` in `~/.codex/config.toml`).
   - **opencode** — approve the `external_directory` prompt on first access, or
     declare the path under `permission.external_directory` / `references`.

Do this the moment a repo enters scope, not at the end: in a Workspace task the
inspection window is how the human follows your work in real time. None of this
needs to be decided up front — a workspace can start (`tmux-agent-workbench new
<feature>`) with zero repos attached and grow into whichever ones the task
actually turns out to touch, one `tmux-agent-workbench add` at a time. An
ordinary session never opts into this window-per-repo behavior implicitly.

Agent-initiated tasks default to the agent's background-task execution mechanism.
Run one-shot builds, tests, lint, installs, and other commands through the agent's
execution tools; do not create tmux panes for them, even when they take a long
time. Track their output and exit status through the agent's task/session handle.
Short commands may finish directly in the execution tool.

Only persistent dev servers in a workbench session use the task-pane workflow
below. Keep the default inspection window unchanged until the dev server is
actually needed. Then run
`tmux-agent-workbench run --name <label> <absolute-repo-path> -- <command> <args...>`.
For commands that require shell syntax, use
`tmux-agent-workbench run --name <label> <absolute-repo-path> --shell '<command>'`.
It creates or reuses a detached task pane in that repo's inspection window
and prints the pane id. Use a stable name such as `dev`;
rerunning the same name replaces the previous live or dead task in the same
pane and retains only the latest output. Use different names only when multiple
persistent dev servers must coexist. Temporary worktrees share their direct workspace member's window
while commands run in the requested worktree directory. Use that pane id with
`tmux capture-pane` to inspect output or `tmux kill-pane` when the task is no
longer needed. Do not run persistent dev servers in the session-level agent pane.
In ordinary sessions, use the agent's background-task mechanism for dev servers
as well, without changing the user's tmux layout.

Missing `TMUX` / `TMUX_PANE` variables do not prove that this is an ordinary
session: agent tool runners may remove them. The command can recover the
session through process ancestry. If a PID sandbox hides that too, use
`WORKBENCH_SESSION=<verified-session-name-or-id>` with the public command.
For `add`, `WORKBENCH_FEATURE=<verified-workspace-name>` is also supported.
If tmux socket access is denied, use the agent's permission mechanism to retry
with socket access. Do not treat a connection or session-resolution failure
as permission to launch a workbench dev server in an independent background
process. This restriction applies only to dev servers that require the workbench
task-pane workflow; other agent tasks use background execution by default.
