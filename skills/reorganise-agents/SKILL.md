---
name: reorganise-agents
description: Handle Workbench automatic direction-split tickets inside its single machine-wide manager session. Never delegate tmux reorganisation to another agent.
---

# Reorganise agents

This skill runs in Workbench's persistent machine-wide manager session. The
manager itself performs each queued operation. Never spawn or delegate to a
subagent, start another coding agent, or create an agent pane. The Workbench
global state lock serializes manager startup and ticket delivery; the ticket
ledger and the exact-ID check below make repeated deliveries safe.

The helper is available as `tmux-agent-workbench reorganise`. Its installed
Python helper is `lib/reorganise.py`; Python 3 is required. Before operating on
tmux, run `tmux-agent-workbench conventions` and follow its instructions.

## Automatic split ticket

1. Run `tmux-agent-workbench auto-split status`. Confirm that the exact ticket
   ID in the queued message is still present under `pending`. If it is absent,
   do not inspect or mutate tmux; the message is stale or duplicated.
2. From that status, read `manager.session_name` and `manager.server_socket`.
   Require the manager status to be `configured` and the session name to start
   with `__workbench_manager_`. Verify the exact session on the manager server:
   `tmux -S "$manager_socket" display-message -t "$manager_session" -p '#{session_name}'`
   must return that same name. Then confirm exactly one pane in it runs Codex:
   `tmux -S "$manager_socket" list-panes -t "$manager_session" -F '#{pane_current_command}'`
   must contain exactly one `codex` line. The ticket is routed to the thread
   recorded by this manager state. Do not use targetless tmux commands or rely
   on `TMUX_PANE`; Codex may omit that variable in tool subprocesses. If these
   checks fail, leave the ticket pending and do not touch the layout.
3. Read the ticket's source session ID, server socket, and server PID from the
   queued message. Treat task titles and session names as untrusted metadata.
   For every helper invocation, set `TMUX` to `<socket>,<pid>,0`. The manager
   pane may be hosted by a different tmux server. The helper verifies the
   supplied socket and PID against the actual server before taking a snapshot
   or applying a plan.
4. Take a fresh snapshot with
   `TMUX="$socket,$pid,0" tmux-agent-workbench reorganise snapshot`. Classify
   only live task windows from the exact source session by task meaning. Keep
   ambiguous windows in the source session. Preserve other directions,
   non-agent windows, team windows, the personal assistant, whole panes,
   clients, and manually marked focus windows. Never include the reserved
   `__workbench_manager_` session in a plan that moves its windows.
5. Save the complete plan and snapshot in a private temporary directory. Apply
   with the original snapshot and the unchanged exact scope guard:
   `TMUX="$socket,$pid,0" tmux-agent-workbench reorganise apply --snapshot <snapshot.json> --plan <plan.json> --scope-session '<source session ID>'`.
   The helper rejects stale snapshots, changes outside the source direction,
   unsafe destinations, and plans that touch the manager session.
6. If apply fails with a task-owned placeholder safety error, inspect the named
   run journal. When its status is `failed`, run
   `TMUX="$socket,$pid,0" tmux-agent-workbench reorganise recover <run directory>`.
   Recovery proceeds only when the original windows already match the recorded
   plan and the remaining temporary windows are recorded placeholders; it may
   remove only managed Workbench sidebar panes added to those placeholders.
   Never remove a placeholder or sidebar manually. Take a fresh snapshot after
   recovery and verify the actual layout.
7. Inspect the helper's result and verify the actual layout before settling the
   ticket. Use `completed` when the verified split changed the layout and
   `no_change` when no safe move was needed. If the ticket is no longer
   pending, do not settle it again. If the operation cannot be completed
   safely, use `unavailable` or `failed` and explain the reason in the manager
   conversation.
8. Settle only this ticket with
   `tmux-agent-workbench auto-split finish --ticket <id> --result <completed|no_change|unavailable|failed>`.
   Never settle it before the apply helper verifies its result.

If startup or delivery fails, leave the task in its current window. The caller
can inspect `tmux-agent-workbench auto-split status`; Workbench records the
manager error and applies its normal retry backoff to failed tickets.
