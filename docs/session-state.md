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

`tmux-agent-workbench title set <name>` names an unnamed task;
`tmux-agent-workbench title rename <name>` updates an existing Workbench name.
Both use the daemon's `task.title` operation. Under the same state lock, it
verifies the live caller and window owner, updates the tmux window name, updates
the agent name and cached window inventory, saves the checkpoint, and publishes
the new snapshot before replying. The sidebar reads that agent name and the
statusbar reads the updated window name. Callers perform one operation.

An explicit CLI name takes precedence over provider hook/index names until the
session changes. This source flag is saved with the checkpoint, and the durable
tmux title rejects stale provider projections. A clear resets the flag together
with the name. A rename retains the original naming options and repair budget.

`tests/session-projections.py` runs a real agent fixture and sidebar on an
isolated tmux server. It checks CLI set/rename against the actual statusbar
format, rendered sidebar, snapshot and checkpoint, including provider refresh
and daemon recovery. It also checks both hook/redraw orders for clear.
`tests/task-titles.py` separately
covers manual names, shared windows, naming budgets and process ownership.
Both suites run through the required `sh tests/verify.sh` command.
