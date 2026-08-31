#!/bin/sh
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
# shellcheck disable=SC1091 # path is resolved from this script at runtime
. "$dir/helpers.sh"

run_task="$dir/../bin/mux-run-task"
public_cli="$dir/../bin/tmux-agent-workbench-cli"
wb_test_setup "wb-test-mux-run-task-$$"
trap wb_test_teardown EXIT

warning_one=$($run_task 2>&1 || true)
warning_two=$($run_task 2>&1 || true)
wb_assert "legacy command warns on first direct invocation" sh -c \
  "printf '%s\\n' \"\$1\" | grep -F 'use \`tmux-agent-workbench run\`' >/dev/null" sh "$warning_one"
wb_assert "legacy command warns on every direct invocation" sh -c \
  "printf '%s\\n' \"\$1\" | grep -F 'use \`tmux-agent-workbench run\`' >/dev/null" sh "$warning_two"

repo="$WB_TEST_TMPDIR/code/web-app"
mkdir -p "$repo"
(cd "$repo" && git init -q)

tmux new-session -d -s target -n main
# Keep exact pane counts deterministic even when the user's real tmux config
# loads tmux-agent-sidebar into the isolated test server.
tmux set-option -g @sidebar_auto_create off
tmux set-option -t target @workbench_task 1

task_pane=$(env -u TMUX WORKBENCH_SESSION=target "$public_cli" run \
  --name dev "$repo" -- sleep 30)

wb_assert "inspection window was created on demand" \
  sh -c "tmux list-windows -t target -F '#W' | grep -qxF web-app"
pane_count=$(tmux list-panes -t target:web-app | wc -l | tr -d ' ')
wb_assert "long task appends one pane to the default three" test "$pane_count" -eq 4
wb_assert "returned task pane exists" tmux display-message -p -t "$task_pane" '#{pane_id}'
wb_assert "task pane is tagged by role" test \
  "$(tmux show-option -pv -t "$task_pane" @pane_role)" = task
wb_assert "task pane records its label" test \
  "$(tmux show-option -pv -t "$task_pane" @workbench_task_name)" = dev
wb_assert "task pane records its command" test \
  "$(tmux show-option -pv -t "$task_pane" @workbench_task_command)" = "'sleep' '30'"

task_process=
attempt=0
while [ "$attempt" -lt 3 ]; do
  task_process=$(tmux display-message -p -t "$task_pane" '#{pane_current_command}')
  [ "$task_process" = sleep ] && break
  sleep 1
  attempt=$((attempt + 1))
done
wb_assert "task command is running" test "$task_process" = sleep

win_width=$(tmux display-message -p -t target:web-app '#{window_width}')
same_width_count=$(tmux list-panes -t target:web-app -F '#{pane_width}' | grep -cxF "$win_width")
wb_assert "all task panes are re-laid out even-vertical" test "$same_width_count" -eq 4

# Adding another detached task must preserve the inspection window's active
# pane instead of selecting the new task pane as a side effect of re-layout.
original_active=$(tmux list-panes -t target:web-app -f '#{pane_active}' -F '#{pane_id}')
second_task=$(env -u TMUX WORKBENCH_SESSION=target "$public_cli" run \
  --name watcher "$repo" -- sleep 30)
active_after=$(tmux list-panes -t target:web-app -f '#{pane_active}' -F '#{pane_id}')
wb_assert "long task preserves the active pane" test "$active_after" = "$original_active"
tmux kill-pane -t "$second_task"

wb_test_report
