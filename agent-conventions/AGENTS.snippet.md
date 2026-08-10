# tmux-agent-workbench conventions (copy into your own agent-instructions file)

tmux has no reach into `~/.claude`, `~/.codex`, or `~/.config/opencode` — this
snippet installs nothing automatically; copy the section below into your own
`CLAUDE.md` / `AGENTS.md` so your agent actually knows the conventions.

---

## tmux task/window model — folding another repo into the task

Some tmux sessions are **task workbenches**: one session is one task, you (the
agent) are the driver living in the `agent` window, and each repo the task
touches gets its own inspection window (git/tig + shell). Such a session is
launched via `mux-agent`, which stamps it with a `@workbench_task` marker. You
may equally be running in an **ordinary** session (a bare agent the human
started ad-hoc); there, this whole mechanism stays out of the way and you do
nothing special for cross-repo work.

When your work starts touching a **local repo other than the one you started in**:

1. First check whether you're in a **workspace** session — its root (and every
   repo already folded in) lives under `~/Workspace/<feature>/...` rather than
   directly under `~/Code`. That's a multi-repo task: each member repo is its
   own git worktree/branch dedicated to this feature, not just a read-only
   peek at the repo's shared `~/Code` checkout.
   - **Workspace session, repo not yet a member** — run
     `ws-add <repo-short-name>[:<branch>]` (e.g. `ws-add web-app`) instead of
     `mux-inspect`. It creates that repo's worktree under this workspace
     (branch defaults to the workspace's own name) *and* folds it in as an
     inspection window in one step. Idempotent — re-running it once the
     worktree exists just re-focuses the window.
   - **Any other session** (an ordinary single-repo session from the `~/Code`
     pool, or a workspace repo that's already a member) — `mux-inspect
     <absolute-repo-path>` as below.
2. Run `mux-inspect <absolute-repo-path>` once (via your shell tool) — either
   directly (non-workspace case) or as the last step `ws-add` already took care
   of. In a workbench it adds that repo as an inspection window in the current
   session, detached — it appears without stealing focus. Idempotent and safe
   to re-run. It **self-gates**: in an ordinary (non-workbench) session it
   simply no-ops, so you can call it unconditionally without worrying about
   which kind of session you are in — no need to detect the session type
   yourself.
3. Bring the repo into your own **write** scope (the mechanism differs per agent):
   - **Claude Code** — `/add-dir <path>` (effective immediately).
   - **Codex** — you can already read it; to write there the session must be
     relaunched with `--add-dir <path>` (or add it to
     `sandbox_workspace_write.writable_roots` in `~/.codex/config.toml`).
   - **opencode** — approve the `external_directory` prompt on first access, or
     declare the path under `permission.external_directory` / `references`.

Do this the moment a repo enters scope, not at the end: the inspection window is
how the human follows your cross-repo work in real time. None of this needs to
be decided up front — a workspace can start (`ws-new <feature>`) with zero
repos attached and grow into whichever ones the task actually turns out to
touch, one `ws-add` at a time.

When a task needs a dev server or another long-running command, keep the default
inspection window unchanged until the command is actually needed. Then run
`mux-run-task --name <label> <absolute-repo-path> '<command>'`. It appends a
detached task pane to that repo's inspection window, starts the command, and
prints the pane id. Use that pane id with `tmux capture-pane` to inspect output
or `tmux kill-pane` when the task is no longer needed. Do not run persistent
project processes in the session-level agent pane.
