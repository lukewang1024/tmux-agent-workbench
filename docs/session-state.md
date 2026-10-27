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
window name, naming options and the repair budget; it must not invent another
foreground session or substitute a locally modified snapshot.

When adding shared session data or a consumer:

1. Define whether the data belongs to the process or to its current session.
2. Put session resets in `bind_session`, with checkpoint recovery where needed.
3. Consume the canonical snapshot in presentation code.
4. Extend the projection contract for any affected observable behavior.

`tests/session-projections.py` runs a real agent fixture and sidebar on an
isolated tmux server. It checks the window, rendered sidebar, snapshot and
checkpoint together for hook-before-redraw and redraw-before-hook clears, a
newly named session, and daemon recovery. `tests/task-titles.py` separately
covers manual names, shared windows, naming budgets and process ownership.
Both suites run through the required `sh tests/verify.sh` command.
