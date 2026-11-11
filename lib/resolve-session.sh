# shellcheck shell=sh
# Resolve the caller, never the currently focused client. Agent tool runners
# may remove TMUX/TMUX_PANE; outside a PID sandbox, ancestry still identifies
# the owning pane. Inside a PID sandbox callers must provide an explicit target.
workbench_resolve_pane() (
  wb_panes=$(tmux list-panes -a -F '#{pane_id} #{pane_pid}') || exit 1
  if [ -n "${TMUX_PANE:-}" ]; then
    # The same window may be linked into multiple sessions; repeated rows for
    # the same pane are still one identity. Use shell builtins on this common
    # explicit path instead of starting awk, sort and wc for every task add.
    wb_target=
    while IFS=' ' read -r wb_id wb_pane_pid; do
      [ "$wb_id" = "$TMUX_PANE" ] || continue
      wb_target=$wb_id
    done <<EOF
$wb_panes
EOF
    [ -n "$wb_target" ] || {
      echo 'workbench: cannot uniquely locate the calling pane; set TMUX_PANE explicitly.' >&2
      exit 1
    }
    printf '%s\n' "$wb_target"
    exit 0
  else
    wb_pid=$$
    wb_target=
    while [ "$wb_pid" -gt 1 ] 2>/dev/null; do
      wb_target=$(printf '%s\n' "$wb_panes" | awk -v pid="$wb_pid" '$2 == pid { print $1 }' | sort -u)
      [ -z "$wb_target" ] || break
      wb_pid=$(ps -o ppid= -p "$wb_pid" 2>/dev/null | tr -d ' ') || break
      case $wb_pid in ''|*[!0-9]*) break ;; esac
    done
  fi
  [ -n "$wb_target" ] && [ "$(printf '%s\n' "$wb_target" | wc -l)" -eq 1 ] || {
    echo 'workbench: cannot uniquely locate the calling pane; set TMUX_PANE explicitly.' >&2
    exit 1
  }
  printf '%s\n' "$wb_target"
)

workbench_resolve_session() (
  wb_sessions=$(tmux list-sessions -F '#{session_id} #{session_name}') || {
    echo 'workbench: cannot connect to tmux; check socket access/sandbox permissions (this does not mean the session is not a workbench).' >&2
    exit 1
  }
  if [ -n "${WORKBENCH_SESSION:-}" ]; then
    wb_target=
    wb_match_count=0
    while IFS= read -r wb_line; do
      wb_id=${wb_line%% *}
      wb_name=${wb_line#* }
      if [ "$wb_id" = "$WORKBENCH_SESSION" ] || [ "$wb_name" = "$WORKBENCH_SESSION" ]; then
        wb_target=$wb_id
        wb_match_count=$((wb_match_count + 1))
      fi
    done <<EOF
$wb_sessions
EOF
    if [ "$wb_match_count" -eq 1 ]; then
      printf '%s\n' "$wb_target"
      exit 0
    fi
    echo "workbench: explicit session not found or ambiguous: $WORKBENCH_SESSION" >&2
    exit 1
  fi

  wb_panes=$(tmux list-panes -a -F '#{pane_id} #{pane_pid} #{session_id}') || exit 1
  if [ -n "${TMUX_PANE:-}" ]; then
    wb_target=$(printf '%s\n' "$wb_panes" | awk -v pane="$TMUX_PANE" '$1 == pane { print $3 }' | sort -u)
    [ -n "$wb_target" ] || {
      echo "workbench: TMUX_PANE is not present on this tmux server: $TMUX_PANE" >&2
      exit 1
    }
  else
    wb_pid=$$
    wb_matches=
    while [ "$wb_pid" -gt 1 ] 2>/dev/null; do
      wb_match=$(printf '%s\n' "$wb_panes" | awk -v pid="$wb_pid" '$2 == pid { print $3 }')
      wb_matches="$wb_matches $wb_match"
      wb_pid=$(ps -o ppid= -p "$wb_pid" 2>/dev/null | tr -d ' ') || break
      case $wb_pid in ''|*[!0-9]*) break ;; esac
    done
    wb_target=$(printf '%s\n' "$wb_matches" | tr ' ' '\n' | sed '/^$/d' | sort -u)
    if [ -z "$wb_target" ] && [ -n "${TMUX:-}" ]; then
      # A running pane can move between sessions while its inherited TMUX
      # value stays unchanged. Prefer live ancestry; use TMUX only as fallback.
      wb_id=${TMUX##*,}
      case $wb_id in
        ''|*[!0-9]*) echo 'workbench: invalid TMUX session identity; set WORKBENCH_SESSION explicitly.' >&2; exit 1 ;;
      esac
      wb_target=$(printf '%s\n' "$wb_sessions" | awk -v id="\$$wb_id" '$1 == id { print $1 }')
    fi
  fi
  if [ -n "$wb_target" ] && [ "$(printf '%s\n' "$wb_target" | wc -l)" -eq 1 ]; then
    printf '%s\n' "$wb_target"
    exit 0
  fi
  echo 'workbench: cannot uniquely locate the calling session; the tool environment or PID sandbox may hide it. Set WORKBENCH_SESSION to the intended session name or ID; do not infer that this is not a workbench.' >&2
  exit 1
)
