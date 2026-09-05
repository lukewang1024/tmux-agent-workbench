#!/bin/sh
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"
. "$dir/../lib/resolve-session.sh"
wb_test_setup "wb-test-resolve-session-$$"
trap wb_test_teardown EXIT
unset TMUX TMUX_PANE WORKBENCH_SESSION

tmux new-session -d -s target -n main
tmux set-option -g @sidebar_auto_create off
expected=$(tmux display-message -p -t target '#{session_id}')
wb_assert 'explicit session resolves without TMUX' test \
  "$(WORKBENCH_SESSION=target workbench_resolve_session)" = "$expected"
wb_assert 'explicit ID resolves' test \
  "$(WORKBENCH_SESSION=$expected workbench_resolve_session)" = "$expected"
wb_assert 'TMUX session identity resolves without pane identity' test \
  "$(TMUX="$(tmux display-message -p -t target '#{socket_path},#{pid}'),${expected#\$}" workbench_resolve_session)" = "$expected"
wb_assert 'pane identity resolves without TMUX' test \
  "$(TMUX_PANE=$(tmux display-message -p -t target '#{pane_id}') workbench_resolve_session)" = "$expected"
if WORKBENCH_SESSION=missing workbench_resolve_session >"$WB_TEST_TMPDIR/error" 2>&1; then
  wb_assert 'missing explicit session fails' false
fi
wb_assert 'missing session is diagnosed' grep -q 'explicit session not found' "$WB_TEST_TMPDIR/error"
if workbench_resolve_session >"$WB_TEST_TMPDIR/error" 2>&1; then
  wb_assert 'outside caller cannot guess an existing session' false
fi
wb_assert 'unresolved caller gets explicit targeting guidance' grep -q 'Set WORKBENCH_SESSION' "$WB_TEST_TMPDIR/error"

# Execute in a real pane with all tmux identity variables removed. The second
# unrelated session ensures detection cannot pass by choosing the only session.
tmux new-session -d -s unrelated
tmux new-window -d -t target -n probe \
  -e "WB_RESOLVER=$dir/../lib/resolve-session.sh" \
  -e "WB_RESULT=$WB_TEST_TMPDIR/result" \
  -e "PATH=$PATH" \
  'exec /bin/sh -c '\''unset TMUX TMUX_PANE WORKBENCH_SESSION; . "$WB_RESOLVER"; workbench_resolve_session > "$WB_RESULT"'\'''
attempt=0
while [ ! -s "$WB_TEST_TMPDIR/result" ] && [ "$attempt" -lt 10 ]; do
  sleep 1
  attempt=$((attempt + 1))
done
wb_assert 'process ancestry recovers correct session' test \
  "$(cat "$WB_TEST_TMPDIR/result")" = "$expected"

# Simulate a socket denial without touching the user's actual server.
tmux() { echo 'Operation not permitted' >&2; return 1; }
if workbench_resolve_session >"$WB_TEST_TMPDIR/error" 2>&1; then
  wb_assert 'socket denial fails' false
fi
unset -f tmux
wb_assert 'socket error is distinguished from session identity' grep -q 'cannot connect to tmux' "$WB_TEST_TMPDIR/error"
wb_test_report
