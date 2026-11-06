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

1. Identify the current agent's own native subagent tool. For **Codex**, run
   the helper with `batch-worker`; it resolves the worker settings from
   `agent-team`'s current Codex background-automation recommendation (or its
   legacy `batch` scene) without launching an agent. Use the returned `model`
   and `reasoning_effort` exactly. Do not substitute another recommendation.
   For **Claude Code, OpenCode, or Trae**, use that same agent's native
   subagent with its own model defaults; do not apply the Codex recommendation.
   OpenCode uses `task`: for automatic splits the Workbench plugin checks
   that the chosen fresh foreground child has no broader permissions than the
   parent. If that check cannot be completed, mark the split unavailable.
   Trae uses `Agent`; wait for its native child to finish before settling a
   split ticket.
2. Spawn exactly one native subagent with fresh context where supported. In
   Codex, use `fork_turns="none"` and the explicit resolved model and effort.
   Preserve inherited execution permissions and proxy environment. A launch
   plan is configuration data, not authorization to execute its argv. If the
   required native tool or settings are unavailable, report that limitation;
   do not launch another CLI or create an interactive pane as a substitute.
3. Give the worker the original request, relevant accepted preferences, the
   resolved helper path, [worker instructions](references/worker.md), and the
   verified tmux server/socket identity. Include scope exclusions and whether
   the user wants execution or a proposal. A user request to reorganise already
   authorizes the reversible layout changes within that scope. Do not invent a
   second approval step. The worker must not spawn further agents.
   An automatic split request identifies one exact source session and a
   required `--scope-session` guard; pass both to the worker unchanged.
4. Wait for the worker's result, continue user-facing progress updates, and
   inspect its grouping, run directory, verification, and cleanup outcome.
   Report actual completion or the concrete blocker. A failed resolver or
   worker does not authorize a retry with a different model.
   For an automatic request, settle its ticket as instructed after verifying
   the worker result, including unavailable or failed attempts.

Keep a user preference such as retaining session `0` as a scratchpad in the
worker brief. Group names and the choice to keep a scratchpad are determined
by the current request and layout, not fixed by this skill. Do not create tmux
panes or another interactive agent window for the subagent.
