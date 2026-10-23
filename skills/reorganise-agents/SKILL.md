---
name: reorganise-agents
description: Classify existing tmux agents by task and regroup their windows into sessions using one native batch subagent. Use for requests to reorganise agents, 整理 agents, or 按性质分 session.
---

# Reorganise agents

Delegate the requested layout operation to exactly one native subagent. The
worker handles discovery, semantic grouping, migration, and verification; the
parent communicates with the user and checks the returned evidence.

Read `tmux-agent-workbench conventions`. Locate this skill's physical directory
(resolve discovery symlinks); its bundled helper is `../../bin/wb-reorganise`.
A matching Workbench installation also exposes that helper as
`tmux-agent-workbench reorganise`. Python 3 and `agent-team` are required.

## Dispatch

1. Run the helper with `batch-worker`. This resolves the **worker** settings of
   `agent-team`'s Codex `batch` scene without launching another CLI. Use its
   returned `model` and `reasoning_effort` exactly. Do not use the scene's lead
   model, inherit the parent's model, or substitute another scene on failure.
2. Spawn one native subagent with fresh context: `fork_turns="none"`, explicit
   `model` and `reasoning_effort` from the resolver. This request authorizes that
   delegation. Preserve inherited execution permissions and proxy environment;
   a launch plan is configuration data, not authorization to execute its argv.
   If the native tool cannot select those settings, report that limitation.
3. Give the worker the original request, relevant accepted preferences, the
   resolved helper path, [worker instructions](references/worker.md), and the
   verified tmux server/socket identity. Include scope exclusions and whether
   the user wants execution or a proposal. A user request to reorganise already
   authorizes the reversible layout changes within that scope. Do not invent a
   second approval step. The worker must not spawn further agents.
4. Wait for the worker's result, continue user-facing progress updates, and
   inspect its grouping, run directory, verification, and cleanup outcome.
   Report actual completion or the concrete blocker. A failed resolver or
   worker does not authorize a retry with a different model.

Keep a user preference such as retaining session `0` as a scratchpad in the
worker brief. Group names and the choice to keep a scratchpad are determined
by the current request and layout, not fixed by this skill. Do not create tmux
panes or another interactive agent window for the subagent.
