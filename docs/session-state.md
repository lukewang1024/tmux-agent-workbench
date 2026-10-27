# Shared session state

The detector's `StateMachine` owns the foreground session for each live agent
process. Lifecycle hooks and Codex OSC title changes both enter the same
`Tracked::bind_session` transition. A clear redraw can identify the new thread
before Codex emits its first hook; until that hook arrives, lifecycle state
remains an estimate from the screen.

The transition clears the previous session's name, label, task progress,
attention, reason and pending display estimates together. Completing a
truncated OSC UUID with the full hook UUID preserves the new task's data.
Remembering the last visible identity prevents a clear hook followed by a stale
OSC title from switching back to the previous session. If several hooks arrive
before a redraw, the immediately preceding hooked identity is also treated as
stale until the current thread appears. Checkpoints retain both identities so
recovery follows the same rule.

The daemon publishes the resulting `AgentSnapshot` to the sidebar, navigation,
notifications and checkpoints. `task_title` projects that snapshot into the
owned tmux window. Its durable tmux option stores naming ownership, the previous
window name, naming options and the repair budget.

`tmux-agent-workbench title set <name>` names a new task or updates an existing
Workbench name. Add `--session <direction>` to group its window in an exact
existing session or create the session. Omit the name to change only the group.
These use the daemon's `task.title` operation. Under the same state lock, it
verifies the live caller and window owner, updates the tmux window name, updates
the agent name and cached window inventory, saves the checkpoint, and publishes
the new snapshot before replying. The sidebar reads that agent name and the
statusbar reads the updated window name. Callers perform one operation.

`task_placement` separates independent interactive agent panes during process
inventory, before their targets are published. The existing title owner (or
oldest agent) keeps the original window and its supporting panes. Later agents
keep their processes, pane IDs and working directories in new windows. Explicit
`@agent_team` layouts are excluded. Grouping moves an independent task window,
retains window metadata, follows clients to their original panes, and removes
the temporary window used to create a missing session. The daemon refreshes all
cached pane targets and session rollups before replying, including after a
partially failed operation. Session grouping never renames a launching session.
Workspace commands resolve moved task windows by their durable task root across
sessions. Inherited launch-session selectors can recover through verified pane
metadata after their original session disappears. Cleanup closes the task's
window wherever it moved, retaining other tasks and shells in the display group.

Hooks provide existing session directions for a semantic choice by the agent;
the CLI owns all layout mutations. A durable per-task grouping marker suppresses
repeated reminders and survives restart; clear resets it with title ownership.

An explicit CLI name takes precedence over provider hook/index names until the
session changes. This source flag is saved with the checkpoint, and the durable
tmux title rejects stale provider projections. A clear resets the flag together
with the name. A rename retains the original naming options and repair budget.

`tests/session-projections.py` runs a real agent fixture and sidebar on an
isolated tmux server. It checks CLI set/rename against the actual statusbar
format, rendered sidebar, snapshot and checkpoint, including provider refresh
and daemon recovery. It also checks both hook/redraw orders for clear.
`tests/task-titles.py` separately
covers manual names, naming budgets and process ownership.
`tests/task-placement.py` covers startup and later pane separation, group reuse
and creation, concurrent commands, client focus, preserved processes/checkouts,
the rendered sidebar, recovery and explicit team layouts. These suites run
through the required `sh tests/verify.sh` command.
