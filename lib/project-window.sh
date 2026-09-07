# shellcheck shell=sh
# sess is resolved by each caller before sourcing this library.
# shellcheck disable=SC2154
# Resolve a workspace member by git identity, never by a worktree basename.
workbench_project_root() (
  requested=$1
  workspace=$(tmux display-message -p -t "$sess" '#{session_path}')
  pool=$(CDPATH='' cd "${WORKBENCH_WORKSPACE_ROOT:-$HOME/Workspace}" 2>/dev/null && pwd -P) || { printf '%s\n' "$requested"; exit; }
  workspace=$(CDPATH='' cd "$workspace" 2>/dev/null && pwd -P) || exit 1
  if [ "$(dirname "$workspace")" != "$pool" ]; then
    printf '%s\n' "$requested"
    exit
  fi
  common=$(git -C "$requested" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 1
  common=$(CDPATH='' cd "$common" && pwd -P)
  for member in "$workspace"/*; do
    [ -d "$member" ] || continue
    [ -e "$member/.git" ] || continue
    candidate=$(git -C "$member" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || continue
    candidate=$(CDPATH='' cd "$candidate" && pwd -P) || continue
    if [ "$candidate" = "$common" ]; then
      CDPATH='' cd "$member" && pwd -P
      exit
    fi
  done
  echo "workbench: no direct workspace member for $requested; use tmux-agent-workbench add first" >&2
  exit 1
)

# Only windows explicitly owned by inspect are eligible for automatic removal.
# Preserve missing-root windows with live task panes, or an agent pane.
workbench_prune_windows() {
  for stale_window in $(tmux list-windows -t "$sess" -F '#{window_id}'); do
    stale_root=$(tmux show-options -wqv -t "$stale_window" @workbench_project_root)
    [ -n "$stale_root" ] || continue
    [ ! -d "$stale_root" ] || continue
    protected=$(tmux list-panes -t "$stale_window" -F '#{?#{||:#{==:#{@pane_role},agent},#{&&:#{==:#{@pane_role},task},#{==:#{pane_dead},0}}},keep,}')
    case $protected in *keep*) continue ;; esac
    tmux kill-window -t "$stale_window"
    echo "workbench: removed stale inspection window $stale_window ($stale_root)" >&2
  done
}
