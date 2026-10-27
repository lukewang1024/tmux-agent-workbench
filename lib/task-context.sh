# shellcheck shell=sh
# Resolve task ownership from durable window metadata, never client focus.
# Callers source resolve-session.sh first. Functions set wb_* context variables.

workbench_valid_name() {
  case $1 in
    ''|.|..|-*|*/*|*:*|*.*)
      echo "workbench: invalid name: $1 (use a single directory name without dots or colons)" >&2
      return 1 ;;
  esac
  if printf '%s' "$1" | LC_ALL=C grep '[[:cntrl:]]' >/dev/null; then
    echo 'workbench: names cannot contain control characters.' >&2
    return 1
  fi
}

workbench_workspace_context() {
  if [ -z "${WORKBENCH_SESSION:-}" ] && [ -n "${WORKBENCH_FEATURE:-}" ]; then
    export WORKBENCH_SESSION=$WORKBENCH_FEATURE
  fi
  wb_caller=$(workbench_resolve_pane 2>/dev/null || true)
  if wb_session=$(workbench_resolve_session 2>/dev/null); then
    :
  elif [ -n "$wb_caller" ] && [ -n "${WORKBENCH_TASK:-}" ] && \
       [ "$(tmux show-option -wqv -t "$wb_caller" @workbench_task_name)" = "$WORKBENCH_TASK" ] && \
       [ -n "$(tmux show-option -wqv -t "$wb_caller" @workbench_workspace_root)" ]; then
    # A managed agent inherits its launch session. That session may disappear
    # after its last task moves; durable caller/task metadata still identifies it.
    wb_session=$(WORKBENCH_SESSION= TMUX_PANE=$wb_caller workbench_resolve_session) || return 1
  else
    workbench_resolve_session >/dev/null
    return 1
  fi
  wb_workspace=
  if [ -n "$wb_caller" ] && [ "$(tmux display-message -p -t "$wb_caller" '#{session_id}')" = "$wb_session" ] && \
     { [ -z "${WORKBENCH_TASK:-}" ] || [ "$(tmux show-option -wqv -t "$wb_caller" @workbench_task_name)" = "$WORKBENCH_TASK" ]; }; then
    wb_workspace=$(tmux show-option -wqv -t "$wb_caller" @workbench_workspace_root)
  fi
  if [ -z "$wb_workspace" ] && [ -n "${WORKBENCH_TASK:-}" ]; then
    for wb_candidate in $(tmux list-windows -t "$wb_session" -F '#{window_id}'); do
      if [ "$(tmux show-option -wqv -t "$wb_candidate" @workbench_task_name)" = "$WORKBENCH_TASK" ]; then
        wb_candidate_root=$(tmux show-option -wqv -t "$wb_candidate" @workbench_workspace_root)
        [ -n "$wb_candidate_root" ] || continue
        if [ -n "$wb_workspace" ] && [ "$wb_workspace" != "$wb_candidate_root" ]; then
          echo 'workbench: task name is ambiguous in this session; run from its owning pane.' >&2
          return 1
        fi
        wb_workspace=$wb_candidate_root
      fi
    done
  fi
  if [ -z "$wb_workspace" ]; then
    wb_workspace=$(tmux show-option -qv -t "$wb_session" @workbench_workspace_root)
  fi
  if [ -z "$wb_workspace" ]; then
    wb_workspace=$(tmux display-message -p -t "$wb_session" '#{session_path}')
    wb_pool=$(CDPATH='' cd "${WORKBENCH_WORKSPACE_ROOT:-$HOME/Workspace}" 2>/dev/null && pwd -P) || return 1
    wb_workspace=$(CDPATH='' cd "$wb_workspace" 2>/dev/null && pwd -P) || return 1
    [ "$(dirname "$wb_workspace")" = "$wb_pool" ] || {
      echo 'workbench: this session has no workspace; create one with tmux-agent-workbench new.' >&2
      return 1
    }
  fi
  wb_workspace=$(CDPATH='' cd "$wb_workspace" && pwd -P) || return 1
  wb_direction=$(basename "$wb_workspace")
  # A display group may contain tasks from different workspaces. Only a
  # workspace's original session may adopt its legacy unnamed drivers.
  if [ "$(tmux show-option -qv -t "$wb_session" @workbench_workspace_root)" != "$wb_workspace" ] && \
     [ -n "$(tmux show-option -wqv -t "${wb_caller:-$wb_session}" @workbench_task_root)" ]; then
    return 0
  fi
  # Existing drivers become the original task without moving or restarting.
  for wb_candidate in $(tmux list-windows -t "$wb_session" -F '#{window_id}'); do
    if [ -z "$(tmux show-option -wqv -t "$wb_candidate" @workbench_task_root)" ] && \
       [ "$(tmux display-message -p -t "$wb_candidate" '#{window_name}')" = agent ]; then
      tmux set-option -w -t "$wb_candidate" @workbench_task_root "$wb_workspace"
      tmux set-option -w -t "$wb_candidate" @workbench_task_name "$wb_direction"
      tmux set-option -w -t "$wb_candidate" @workbench_task_started 1
    fi
  done
}

workbench_task_window() {
  wb_window=
  for wb_candidate in $(tmux list-windows -a -F '#{window_id}' | sort -u); do
    if [ "$(tmux show-option -wqv -t "$wb_candidate" @workbench_task_root)" = "$wb_task_root" ]; then
      [ -z "$wb_window" ] || {
        echo "workbench: multiple windows own task $wb_task_root" >&2
        return 1
      }
      wb_window=$wb_candidate
    fi
  done
  if [ -n "$wb_window" ]; then
    wb_session=$(tmux display-message -p -t "$wb_window" '#{session_id}')
  fi
}

workbench_task_context() {
  workbench_workspace_context || return 1
  wb_task=${1:-${WORKBENCH_TASK:-}}
  wb_window=
  if [ -z "$wb_task" ]; then
    wb_caller=$(workbench_resolve_pane 2>/dev/null || true)
    if [ -n "$wb_caller" ] && [ "$(tmux display-message -p -t "$wb_caller" '#{session_id}')" = "$wb_session" ]; then
      wb_task_root=$(tmux show-option -wqv -t "$wb_caller" @workbench_task_root)
      if [ -n "$wb_task_root" ]; then
        wb_task=$(tmux show-option -wqv -t "$wb_caller" @workbench_task_name)
      elif [ "$(tmux display-message -p -t "$wb_caller" '#{window_name}')" = agent ]; then
        # Adopt an existing session-level driver as its original legacy task.
        wb_task=$wb_direction
        tmux set-option -w -t "$wb_caller" @workbench_task_root "$wb_workspace"
        tmux set-option -w -t "$wb_caller" @workbench_task_name "$wb_task"
      fi
    fi
  fi
  if [ -n "$wb_task" ]; then
    workbench_valid_name "$wb_task" || return 1
    wb_task_root=$wb_workspace/tasks/$wb_task
    if [ "$wb_task" = "$wb_direction" ] && [ ! -d "$wb_task_root" ]; then
      for wb_candidate in $(tmux list-windows -a -F '#{window_id}' | sort -u); do
        if [ "$(tmux show-option -wqv -t "$wb_candidate" @workbench_task_root)" = "$wb_workspace" ]; then
          wb_task_root=$wb_workspace
          break
        fi
      done
    fi
    [ -d "$wb_task_root" ] && [ ! -L "$wb_task_root" ] || {
      echo "workbench: task does not exist: $wb_task; use tmux-agent-workbench task new." >&2
      return 1
    }
    workbench_task_window || return 1
    return 0
  fi

  # An explicit session is sufficient only when it contains a single task.
  wb_count=0
  for wb_candidate in $(tmux list-windows -t "$wb_session" -F '#{window_id}'); do
    if [ "$(tmux show-option -wqv -t "$wb_candidate" @workbench_task_root)" = "$wb_workspace" ]; then
      wb_count=$((wb_count + 1))
      wb_task_root=$wb_workspace
    fi
  done
  for wb_candidate in "$wb_workspace"/tasks/*; do
    [ -d "$wb_candidate" ] && [ ! -L "$wb_candidate" ] || continue
    wb_count=$((wb_count + 1))
    wb_task_root=$wb_candidate
  done
  if [ "$wb_count" -eq 1 ]; then
    if [ "$wb_task_root" = "$wb_workspace" ]; then wb_task=$wb_direction
    else wb_task=$(basename "$wb_task_root")
    fi
    workbench_task_window
  elif [ "$wb_count" -gt 1 ]; then
    echo 'workbench: multiple tasks in this session; set WORKBENCH_TASK or run from the owning window.' >&2
    return 1
  else
    # Existing workspaces remain one legacy task; never move their checkouts.
    wb_task=$wb_direction
    wb_task_root=$wb_workspace
    wb_window=$(tmux list-windows -t "$wb_session" -F '#{window_id} #{window_name}' |
      awk '$2 == "agent" {print $1}')
    [ -n "$wb_window" ] || {
      echo 'workbench: no task window; use tmux-agent-workbench task new <name>.' >&2
      return 1
    }
    tmux set-option -w -t "$wb_window" @workbench_task_root "$wb_task_root"
    tmux set-option -w -t "$wb_window" @workbench_task_name "$wb_task"
  fi
}

workbench_task_repo() {
  wb_requested=$(CDPATH='' cd "$1" && pwd -P) || return 1
  if [ "$wb_requested" = "$wb_task_root" ]; then
    printf '%s\n' "$wb_requested"
    return 0
  fi
  if [ -e "$wb_task_root/.git" ]; then
    case $wb_requested in
      "$wb_task_root"/tasks/*) : ;;
      "$wb_task_root"/*) printf '%s\n' "$wb_requested"; return 0 ;;
    esac
  fi
  # Tools and processes may only use checkouts owned by this task.
  for wb_member in "$wb_task_root"/*; do
    [ -d "$wb_member" ] && [ ! -L "$wb_member" ] && [ -e "$wb_member/.git" ] || continue
    wb_member=$(CDPATH='' cd "$wb_member" && pwd -P) || continue
    case $wb_requested in "$wb_member"|"$wb_member"/*) printf '%s\n' "$wb_requested"; return 0 ;; esac
  done
  echo "workbench: $wb_requested is not a checkout owned by task $wb_task; add the repository to this task first." >&2
  return 1
}

# tmux's server environment may predate the launching agent's proxy settings.
# Copy configured proxies to the actual session environment before starting work.
workbench_export_proxies() {
  for wb_key in HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY; do
    wb_lower=$(printf '%s' "$wb_key" | tr '[:upper:]' '[:lower:]')
    wb_value=$(printenv "$wb_key" 2>/dev/null || printenv "$wb_lower" 2>/dev/null || true)
    if [ -n "$wb_value" ]; then
      export "$wb_key=$wb_value" "$wb_lower=$wb_value"
    fi
  done
}

workbench_inherit_proxies() {
  workbench_export_proxies
  for wb_key in HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY http_proxy https_proxy all_proxy no_proxy; do
    wb_value=$(printenv "$wb_key" 2>/dev/null || true)
    [ -z "$wb_value" ] || tmux set-environment -t "$wb_session" "$wb_key" "$wb_value"
  done
}
