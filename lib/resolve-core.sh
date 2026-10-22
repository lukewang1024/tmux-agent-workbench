#!/bin/sh
# Shared by the CLI, plugin and UI launchers. An installed release supersedes
# checkout builds unless the user explicitly selects a development binary.
workbench_core_path()
{
  if [ -n "${TMUX_AGENT_WORKBENCH_BIN:-}" ]; then
    [ -x "$TMUX_AGENT_WORKBENCH_BIN" ] || return 1
    printf '%s\n' "$TMUX_AGENT_WORKBENCH_BIN"
  elif [ -x "${XDG_DATA_HOME:-$HOME/.local/share}/tmux-agent-workbench/bin/tmux-agent-workbench-core" ]; then
    printf '%s\n' "${XDG_DATA_HOME:-$HOME/.local/share}/tmux-agent-workbench/bin/tmux-agent-workbench-core"
  elif [ -x "$1/target/release/tmux-agent-workbench" ]; then
    printf '%s\n' "$1/target/release/tmux-agent-workbench"
  else
    return 1
  fi
}
