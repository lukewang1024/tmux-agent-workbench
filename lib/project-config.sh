#!/bin/sh
# Shared project-layout metadata consumed by gen-project-configs,
# gen-tmuxinator-configs, and mux-inspect.

project_config_name() {
  pc_path=$1
  pc_primary=$(git -C "$pc_path" worktree list --porcelain 2>/dev/null |
    sed -n 's/^worktree //p' | head -1)
  [ -n "$pc_primary" ] || pc_primary=$pc_path
  basename "$pc_primary" | tr ' .:/' '____'
}

project_config_dir() {
  printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/tmux-agent-workbench/projects"
}

project_config_path() {
  pc_name=$(project_config_name "$1")
  printf '%s/%s.conf\n' "$(project_config_dir)" "$pc_name"
}

# Load the fixed, data-only key set. The file is deliberately parsed rather
# than sourced: a command such as `pnpm dev -- --host=0.0.0.0` stays data and
# cannot execute while a layout is being resolved.
# shellcheck disable=SC2034
project_config_load() {
  # Exported-by-sourcing API; consumers read these after the function returns.
  PROJECT_LAYOUT=even-vertical
  PROJECT_DEV_COMMAND=
  pc_file=$(project_config_path "$1")
  [ -f "$pc_file" ] || return 0

  while IFS='=' read -r pc_key pc_value || [ -n "$pc_key$pc_value" ]; do
    case $pc_key in
      layout) [ -n "$pc_value" ] && PROJECT_LAYOUT=$pc_value ;;
      dev_command) PROJECT_DEV_COMMAND=$pc_value ;;
    esac
  done < "$pc_file"

  case $PROJECT_LAYOUT in
    even-horizontal|even-vertical|main-horizontal|main-horizontal-mirrored|main-vertical|main-vertical-mirrored|tiled) : ;;
    *) PROJECT_LAYOUT=even-vertical ;;
  esac
}
