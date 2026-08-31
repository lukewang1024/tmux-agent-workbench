#!/bin/sh
set -eu

repo=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
binary=$repo/target/debug/tmux-agent-workbench
test_root=$(mktemp -d "${TMPDIR:-/tmp}/workbench-v2-test.XXXXXX")
socket=$test_root/tmux.sock

cleanup()
{
  tmux -S "$socket" kill-server >/dev/null 2>&1 || true
  if [ -n "${socket_bootstrap:-}" ]; then
    tmux -S "$socket_bootstrap" kill-server >/dev/null 2>&1 || true
  fi
  rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

wait_sidebar_state()
{
  sidebar_target=$1
  sidebar_expected=$2
  sidebar_tries=0
  while :; do
    if tmux -S "$socket" list-panes -t "$sidebar_target" -F '#{@pane_role}' | \
      grep '^sidebar$' >/dev/null; then
      [ "$sidebar_expected" = present ] && return 0
    else
      [ "$sidebar_expected" = absent ] && return 0
    fi
    sidebar_tries=$((sidebar_tries + 1))
    [ "$sidebar_tries" -lt 40 ] || return 1
    sleep 0.1
  done
}

export XDG_CONFIG_HOME="$test_root/config"
export XDG_STATE_HOME="$test_root/state"
export XDG_CACHE_HOME="$test_root/cache"
export XDG_RUNTIME_DIR="$test_root/runtime"
export TMUX_AGENT_WORKBENCH_TMUX_SOCKET="$socket"
export TMUX_AGENT_WORKBENCH_BIN="$binary"

cargo build --quiet --manifest-path "$repo/Cargo.toml" --bin tmux-agent-workbench
cargo build --quiet --manifest-path "$repo/Cargo.toml" --example codex
tmux -f "$repo/tests/fixtures/tmux.conf" -S "$socket" \
  new-session -d -s workbench-test -x 140 -y 40

"$binary" daemon ensure >/dev/null
first_pid=$("$binary" daemon status | sed -n 's/.*"pid": *\([0-9][0-9]*\).*/\1/p')
"$binary" daemon ensure >/dev/null
second_pid=$("$binary" daemon status | sed -n 's/.*"pid": *\([0-9][0-9]*\).*/\1/p')
[ -n "$first_pid" ] && [ "$first_pid" = "$second_pid" ]

# Lifecycle hooks may fire before the asynchronous plugin startup has created
# its socket, or after a daemon crash. `wake` must recover the daemon rather
# than returning a tmux run-shell failure.
"$binary" daemon stop >/dev/null
tries=0
while "$binary" daemon status >/dev/null 2>&1; do
  tries=$((tries + 1))
  [ "$tries" -lt 60 ] || exit 1
  sleep 0.05
done
"$binary" daemon wake
"$binary" daemon status >/dev/null

"$binary" snapshot --json | grep '"schema_version": 1' >/dev/null

mkdir -p "$XDG_CONFIG_HOME/tmux-agent-workbench"
printf '%s\n' '[detection]' 'process_interval_ms = 750' > \
  "$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml"
"$binary" reload >/dev/null
printf '%s\n' 'unknown_key = true' > \
  "$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml"
# Beta configuration is additive: preserve/ignore forward-compatible unknown
# keys instead of preventing the daemon from reloading known settings.
"$binary" reload >/dev/null
"$binary" daemon status >/dev/null
rm "$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml"

mkdir -p "$XDG_CONFIG_HOME/tmux-agent-workbench/manifests"
printf '%s\n' 'version = 99' > \
  "$XDG_CONFIG_HOME/tmux-agent-workbench/manifests/codex.toml"
if "$binary" reload >/dev/null 2>&1; then
  exit 1
fi
"$binary" daemon status >/dev/null
rm "$XDG_CONFIG_HOME/tmux-agent-workbench/manifests/codex.toml"

# Simulate a previous release that owned prefix+a, followed by the user's
# config reclaiming it before this plugin reload. Migration must preserve the
# user binding while retiring the Agent picker's old global default.
tmux -S "$socket" set-option -g @workbench-_bound-agent a
tmux -S "$socket" bind-key -N 'user broadcast' a display-message broadcast
tmux -S "$socket" run-shell "bash '$repo/workbench.tmux'"
sleep 1
tmux -S "$socket" list-keys -T prefix -N | grep 'toggle Workbench agent sidebar$' >/dev/null
if tmux -S "$socket" list-keys -T prefix -N | \
  grep -E 'pick tmuxinator workbench project|new workspace \(feature \+ repos\)|pick agent$' >/dev/null; then
  exit 1
fi
tmux -S "$socket" list-keys -T prefix | grep 'display-message broadcast' >/dev/null
initial_window=$(tmux -S "$socket" display-message -p -t workbench-test:1 '#{window_id}')
wait_sidebar_state workbench-test:1 present

# An explicit toggle-off survives maintenance; toggling again opts back in.
"$binary" sidebar-control toggle "$initial_window"
sleep 1
if tmux -S "$socket" list-panes -t workbench-test:1 -F '#{@pane_role}' | \
  grep '^sidebar$' >/dev/null; then
  exit 1
fi
"$binary" sidebar-control maintain "$initial_window"
if tmux -S "$socket" list-panes -t workbench-test:1 -F '#{@pane_role}' | \
  grep '^sidebar$' >/dev/null; then
  exit 1
fi
"$binary" sidebar-control toggle "$initial_window"
tmux -S "$socket" list-panes -t workbench-test:1 -F '#{@pane_role}' | \
  grep '^sidebar$' >/dev/null

tmux -S "$socket" new-window -d -n sidebar-check
wait_sidebar_state sidebar-check present
tmux -S "$socket" list-keys -T prefix | grep 'wb-responsive' >/dev/null
tmux -S "$socket" list-panes -a -F '#{@pane_role}' | grep '^sidebar$' >/dev/null
tmux -S "$socket" resize-window -t sidebar-check -x 100
wait_sidebar_state sidebar-check absent

# An unobserved background window intentionally stays hidden after growing:
# aggressive-resize may otherwise recreate it at the server's fallback size
# only to destroy it again when a narrow client returns. Attach a real control
# client and make sidebar-check visible before asserting responsive restore.
responsive_fifo=$test_root/responsive-client.in
mkfifo "$responsive_fifo"
exec 7<>"$responsive_fifo"
tmux -S "$socket" -C attach-session -t workbench-test \
  <"$responsive_fifo" >"$test_root/responsive-client.out" 2>&1 &
responsive_client_pid=$!
tries=0
while [ "$(tmux -S "$socket" list-clients -F '#{client_name}' | wc -l | tr -d ' ')" -lt 1 ]; do
  tries=$((tries + 1))
  [ "$tries" -lt 60 ] || exit 1
  sleep 0.05
done
responsive_client=$(tmux -S "$socket" list-clients -F '#{client_name}' | sed -n '1p')
tmux -S "$socket" switch-client -c "$responsive_client" -t workbench-test:sidebar-check
tmux -S "$socket" refresh-client -t "$responsive_client" -C 140x40
tmux -S "$socket" resize-window -t sidebar-check -x 140
wait_sidebar_state sidebar-check present
kill "$responsive_client_pid" 2>/dev/null || true
exec 7>&-

# Repeated sidebar open/close must not distort the main pane proportions.
tmux -S "$socket" new-window -d -n layout-preserve
sleep 1
layout_window=$(tmux -S "$socket" display-message -p -t layout-preserve '#{window_id}')
"$binary" sidebar-control toggle "$layout_window"
tmux -S "$socket" split-window -h -t layout-preserve
tmux -S "$socket" select-layout -t layout-preserve even-horizontal >/dev/null
before_widths=$(tmux -S "$socket" list-panes -t layout-preserve \
  -F '#{pane_width}' | paste -sd, -)
"$binary" sidebar-control toggle "$layout_window"
"$binary" sidebar-control toggle "$layout_window"
after_widths=$(tmux -S "$socket" list-panes -t layout-preserve \
  -F '#{pane_width}' | paste -sd, -)
[ "$before_widths" = "$after_widths" ]
"$binary" sidebar-control toggle "$layout_window"
sidebar_width=$(tmux -S "$socket" list-panes -t layout-preserve \
  -f '#{==:#{@pane_role},sidebar}' -F '#{pane_width}')
[ "$sidebar_width" = 26 ]

# Resurrect snapshots must contain only user panes and the main-only layout.
resurrect_file=$test_root/resurrect.txt
tab=$(printf '\t')
tmux -S "$socket" list-panes -t layout-preserve \
  -F "pane${tab}#{session_name}${tab}#{window_index}${tab}0${tab}:-${tab}#{pane_index}${tab}title${tab}:/tmp${tab}#{pane_active}${tab}#{pane_current_command}${tab}:" \
  > "$resurrect_file"
tmux -S "$socket" display-message -p -t layout-preserve \
  -F "window${tab}#{session_name}${tab}#{window_index}${tab}:name${tab}0${tab}:-${tab}#{window_layout}${tab}off" \
  >> "$resurrect_file"
TMUX_AGENT_WORKBENCH_TMUX_SOCKET=$socket \
  "$repo/bin/workbench-resurrect-save-hook" "$resurrect_file"
[ "$(grep -c '^pane' "$resurrect_file")" = 2 ]
saved_layout=$(grep '^window' "$resurrect_file" | cut -f7)
main_layout=$(tmux -S "$socket" show-window-options -v \
  -t layout-preserve @workbench_main_layout)
[ "$saved_layout" = "$main_layout" ]

# Adding a workspace pane while the sidebar is open invalidates the saved main
# layout's pane count. Closing remains successful and preserves the new pane.
tmux -S "$socket" split-window -v -t layout-preserve
"$binary" sidebar-control toggle "$layout_window"
[ "$(tmux -S "$socket" list-panes -t layout-preserve -F '#{pane_id}' | wc -l | tr -d ' ')" = 3 ]

printf '%s\n' '[sidebar]' 'auto_create = false' > \
  "$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml"
"$binary" reload >/dev/null
tmux -S "$socket" new-window -d -n no-auto-sidebar
sleep 1
if tmux -S "$socket" list-panes -t no-auto-sidebar -F '#{@pane_role}' | \
  grep '^sidebar$' >/dev/null; then
  tmux -S "$socket" display-message -p \
    'unexpected auto sidebar: auto=#{@sidebar_auto_create} seeded=#{@workbench_seeded_sidebar_auto_create}' >&2
  tmux -S "$socket" list-panes -t no-auto-sidebar \
    -F 'pane=#{pane_id} role=#{@pane_role} command=#{pane_current_command}' >&2
  exit 1
fi
rm "$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml"
"$binary" reload >/dev/null

tmux -S "$socket" new-window -d -n sole-close
sleep 1
work_pane=$(tmux -S "$socket" list-panes -t sole-close \
  -f '#{!=:#{@pane_role},sidebar}' -F '#{pane_id}' | sed -n '1p')
tmux -S "$socket" select-window -t sole-close
tmux -S "$socket" kill-pane -t "$work_pane"
sleep 1
if tmux -S "$socket" list-windows -F '#{window_name}' | grep '^sole-close$' >/dev/null; then
  exit 1
fi

# Relay click focus runs from SSH without a current tmux client. Exercise exact
# pane targeting and the expired-pane/session fallback against a control client.
focus_fifo=$test_root/focus-client.in
mkfifo "$focus_fifo"
exec 8<>"$focus_fifo"
tmux -S "$socket" -C attach-session -t workbench-test \
  <"$focus_fifo" >"$test_root/focus-client.out" 2>&1 &
focus_client_pid=$!
tmux -S "$socket" new-session -d -s relay-target -x 140 -y 40
target_pane=$(tmux -S "$socket" split-window -d -t relay-target -P -F '#{pane_id}')
target_session=$(tmux -S "$socket" display-message -p -t relay-target '#{session_id}')
sleep 1
"$binary" relay focus-target --tmux-socket "$socket" \
  --session-id "$target_session" --pane-id "$target_pane"
tmux -S "$socket" list-clients -F '#{pane_id}' | grep "^$target_pane$" >/dev/null
tmux -S "$socket" kill-pane -t "$target_pane"
"$binary" relay focus-target --tmux-socket "$socket" \
  --session-id "$target_session" --pane-id %999999
tmux -S "$socket" list-clients -F '#{client_session}' | \
  grep '^relay-target$' >/dev/null

# Picker integration replaces only the human fzf selection. Inventory,
# process discovery, socket snapshot, and exact focus all remain real.
mkdir -p "$test_root/picker-bin"
cp "$repo/tests/fixtures/fake-fzf-tmux" "$test_root/picker-bin/fzf-tmux"
chmod 755 "$test_root/picker-bin/fzf-tmux"
tmux -S "$socket" new-session -d -s agent-pick -x 140 -y 40 \
  "$repo/target/debug/examples/codex"
tmux -S "$socket" rename-window -t agent-pick:1 agent-pick
agent_pane=$(tmux -S "$socket" display-message -p -t agent-pick:1 '#{pane_id}')
tries=0
while ! "$binary" agent explain "$agent_pane" >/dev/null 2>&1; do
  tries=$((tries + 1))
  [ "$tries" -lt 60 ] || exit 1
  sleep 0.05
done
WORKBENCH_PICK_MATCH=relay-target PATH=$test_root/picker-bin:$PATH \
  "$binary" pick session
tmux -S "$socket" list-clients -F '#{client_session}' | \
  grep '^relay-target$' >/dev/null
WORKBENCH_PICK_MATCH=agent-pick PATH=$test_root/picker-bin:$PATH \
  "$binary" pick agent
tmux -S "$socket" list-clients -F '#{pane_id}' | grep "^$agent_pane$" >/dev/null

# Clicking a sidebar makes it active before the handler runs. A cross-window
# jump must restore the source window's previous pane, otherwise returning to
# that window lands in the sidebar itself.
tmux -S "$socket" new-session -d -s source-restore -x 140 -y 40
source_main=$(tmux -S "$socket" display-message -p -t source-restore '#{pane_id}')
source_sidebar=$(tmux -S "$socket" split-window -h -t source-restore -P -F '#{pane_id}')
tmux -S "$socket" select-pane -t "$source_main"
tmux -S "$socket" select-pane -t "$source_sidebar"
"$binary" focus --session "$(tmux -S "$socket" display-message -p -t agent-pick '#{session_id}')" \
  --window "$(tmux -S "$socket" display-message -p -t "$agent_pane" '#{window_id}')" \
  --pane "$agent_pane" --source-pane "$source_sidebar"
tmux -S "$socket" list-panes -t source-restore -f '#{pane_active}' -F '#{pane_id}' | \
  grep "^$source_main$" >/dev/null

# Session rows remain actionable even when no last-active non-sidebar pane is
# available: the hidden focus command accepts a session-only target.
tmux -S "$socket" new-session -d -s session-only
session_only_id=$(tmux -S "$socket" display-message -p -t session-only '#{session_id}')
"$binary" focus --session "$session_only_id"
tmux -S "$socket" list-clients -F '#{session_id}' | grep "^$session_only_id$" >/dev/null

tmux -S "$socket" kill-server
exec 8>&-
wait "$focus_client_pid" 2>/dev/null || true
tries=0
while "$binary" daemon status >/dev/null 2>&1; do
  tries=$((tries + 1))
  [ "$tries" -lt 100 ] || exit 1
  sleep 0.05
done

# A fresh server must be able to bootstrap its daemon solely from plugin
# sourcing. In particular, the daemon must outlive tmux's short run-shell job.
socket_bootstrap=$test_root/bootstrap.sock
tmux -S "$socket_bootstrap" new-session -d -s bootstrap-test -x 140 -y 40
tmux -S "$socket_bootstrap" set-environment -gu TMUX_AGENT_WORKBENCH_TMUX_SOCKET
tmux -S "$socket_bootstrap" run-shell "bash '$repo/workbench.tmux'"
tries=0
while :; do
  if TMUX_AGENT_WORKBENCH_TMUX_SOCKET=$socket_bootstrap \
    "$binary" daemon status >"$test_root/bootstrap-status" 2>/dev/null; then
    break
  fi
  tries=$((tries + 1))
  [ "$tries" -lt 60 ] || exit 1
  sleep 0.05
done
bootstrap_pid=$(sed -n 's/.*"pid": *\([0-9][0-9]*\).*/\1/p' \
  "$test_root/bootstrap-status")
[ -n "$bootstrap_pid" ]
sleep 1
kill -0 "$bootstrap_pid"
tmux -S "$socket_bootstrap" kill-server
tries=0
while TMUX_AGENT_WORKBENCH_TMUX_SOCKET=$socket_bootstrap \
  "$binary" daemon status >/dev/null 2>&1; do
  tries=$((tries + 1))
  [ "$tries" -lt 100 ] || exit 1
  sleep 0.05
done

printf '%s\n' 'isolated tmux integration: ok'
