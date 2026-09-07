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

# mux-run-task deliberately keeps /bin/sh as the pane process while it runs
# the reconstructed command. The foreground child may therefore be `sleep`
# while pane_current_command remains `sh`; pane liveness is the stable tmux
# contract to assert here.
sleep 1
task_dead=$(tmux display-message -p -t "$task_pane" '#{pane_dead}')
wb_assert "task pane remains alive while the command runs" test "$task_dead" = 0

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

replacement=$(env -u TMUX WORKBENCH_SESSION=target "$public_cli" run --name dev "$repo" -- true)
wb_assert "live task is replaced in the same pane" test "$replacement" = "$task_pane"
sleep 1
wb_assert "instant replacement output remains available" test "$(tmux display-message -p -t "$replacement" '#{pane_dead}')" = 1
replacement=$(env -u TMUX WORKBENCH_SESSION=target "$public_cli" run --name dev "$repo" -- sleep 30)
wb_assert "dead task is reused in place" test "$replacement" = "$task_pane"
fast=$(env -u TMUX WORKBENCH_SESSION=target "$public_cli" run --name fast "$repo" -- true)
sleep 1
wb_assert "new instant task is retained" test "$(tmux display-message -p -t "$fast" '#{pane_dead}')" = 1

# Workspace worktrees share the direct member's window and task slot.
workspace="$WORKBENCH_WORKSPACE_ROOT/feature"
mkdir -p "$workspace"
member="$workspace/web-app"
git -C "$repo" -c user.name=Test -c user.email=test@example.com commit --allow-empty -qm init
git -C "$repo" worktree add -qb feature "$member"
temporary="$workspace/.worktrees/arbitrary-name"
git -C "$repo" worktree add -qb temporary "$temporary"
tmux new-session -d -s workspace -n agent -c "$workspace"
tmux set-option -t workspace @workbench_task 1
first=$(env -u TMUX WORKBENCH_SESSION=workspace "$public_cli" run --name dev "$member" -- sleep 30)
second=$(env -u TMUX WORKBENCH_SESSION=workspace "$public_cli" run --name dev "$temporary" -- sleep 30)
wb_assert "temporary worktree shares member task slot" test "$first" = "$second"
wb_assert "temporary worktree does not add a window" test "$(tmux list-windows -t workspace | wc -l | tr -d ' ')" = 2
wb_assert "task runs in the requested worktree" test "$(tmux display-message -p -t "$second" '#{pane_current_path}')" = "$temporary"

# Model a legacy inspection window whose worktree was deleted.
stale="$workspace/.worktrees/deleted"
mkdir -p "$stale"
stale_id=$(tmux new-window -d -t workspace -n stale -c "$stale" -P -F '#{window_id}')
tmux set-option -w -t "$stale_id" @workbench_project_root "$stale"
rmdir "$stale"
env -u TMUX WORKBENCH_SESSION=workspace "$public_cli" prune
wb_assert "prune removes missing-root inspection windows" test "$(tmux list-windows -t workspace | wc -l | tr -d ' ')" = 2
# Missing directories alone must not terminate an active task.
protected=$(tmux new-window -d -t workspace -n protected -P -F '#{pane_id}' 'sleep 30')
tmux set-option -w -t "$protected" @workbench_project_root "$stale"
tmux set-option -p -t "$protected" @pane_role task
env -u TMUX WORKBENCH_SESSION=workspace "$public_cli" prune
wb_assert "prune protects a live task in a missing-root window" test "$(tmux display-message -p -t "$protected" '#{pane_dead}')" = 0
tmux kill-pane -t "$protected"

unrelated="$WB_TEST_TMPDIR/unrelated"
git init -q "$unrelated"
if env -u TMUX WORKBENCH_SESSION=workspace "$public_cli" inspect "$unrelated" >/dev/null 2>&1; then rejected=0; else rejected=1; fi
wb_assert "workspace rejects repositories without a direct member" test "$rejected" = 1

env -u TMUX WORKBENCH_SESSION=target "$public_cli" prune --dead-tasks
wb_assert "explicit prune retains the live dev task" test "$(tmux display-message -p -t "$task_pane" '#{pane_dead}')" = 0
wb_assert "explicit prune removes dead task panes" test "$(tmux list-panes -t target:web-app | wc -l | tr -d ' ')" = 4

wb_test_report
