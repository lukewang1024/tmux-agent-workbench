#!/bin/sh
# test-mux-inspect.sh — exercises mux-inspect targeting a session other than
# the caller's current one via WORKBENCH_SESSION, with the caller not even
# inside tmux ($TMUX unset). Covers:
#   - WORKBENCH_SESSION lets a caller with no $TMUX at all target a session
#   - @workbench_task=1 gate is satisfied, window gets built: 3 panes,
#     even-vertical layout
#   - idempotency: calling again for the same repo does not create a
#     duplicate window
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

bin_dir=$(cd "$dir/../bin" && pwd)
mux_inspect="$bin_dir/mux-inspect"

wb_test_setup "wb-test-mux-inspect-$$"
trap wb_test_teardown EXIT
XDG_CONFIG_HOME="$WB_TEST_TMPDIR/config"
export XDG_CONFIG_HOME

# 1) fake git repo dir
repo="$WB_TEST_TMPDIR/code/somerepo"
mkdir -p "$repo"
(cd "$repo" && git init -q)

# 2) create session "target" in the isolated server and mark it a workbench
# task. Give it a first window so we can later assert "2 windows total"
# (original + the inspection window) without relying on the initial window's
# default name.
tmux new-session -d -s target -n main
tmux set-option -t target @workbench_task 1

# 3) call mux-inspect with WORKBENCH_SESSION=target and TMUX unset — this
# must work without the caller being inside tmux at all.
if out=$(env -u TMUX WORKBENCH_SESSION=target "$mux_inspect" "$repo" --focus --force 2>&1); then
  rc=0
else
  rc=$?
fi
wb_assert "mux-inspect exited 0" test "$rc" -eq 0
[ "$rc" -eq 0 ] || echo "output: $out"

name=$(basename "$repo" | tr ' .:/' '____')

# 4) window exists in "target" with 3 panes, even-vertical layout
wb_assert "inspection window exists in target" \
  sh -c "tmux list-windows -t target -F '#W' | grep -qxF '$name'"

pane_count=$(tmux list-panes -t "target:$name" | wc -l | tr -d ' ')
wb_assert "inspection window has 3 panes" test "$pane_count" -eq 3

# even-vertical layout in tmux is a top-level vertical split; verify via
# the pane geometry instead of pattern-matching the layout checksum string:
# each pane should span the full window width and be stacked (distinct
# top_y offsets), which is what select-layout even-vertical produces.
win_width=$(tmux display-message -p -t "target:$name" '#{window_width}')
same_width_count=$(tmux list-panes -t "target:$name" -F '#{pane_width}' | grep -cxF "$win_width")
wb_assert "all 3 panes span the full window width (even-vertical)" \
  test "$same_width_count" -eq 3

distinct_top=$(tmux list-panes -t "target:$name" -F '#{pane_top}' | sort -u | wc -l | tr -d ' ')
wb_assert "3 panes stacked at distinct vertical offsets (even-vertical)" \
  test "$distinct_top" -eq 3

# focused window should be the inspection window ("--focus" requested it)
active=$(tmux display-message -p -t target '#{window_name}')
wb_assert "--focus selected the inspection window" test "$active" = "$name"

# 5) idempotent case: call again for the same repo — no duplicate window.
env -u TMUX WORKBENCH_SESSION=target "$mux_inspect" "$repo" --focus --force >/dev/null 2>&1 || true

win_count=$(tmux list-windows -t target | wc -l | tr -d ' ')
wb_assert "still exactly 2 windows total (original + inspection, no dup)" \
  test "$win_count" -eq 2

matching_count=$(tmux list-windows -t target -F '#W' | grep -xc "$name" || true)
wb_assert "exactly one window named $name (no duplicate)" test "$matching_count" -eq 1

# 6) A manually edited project config adds a fourth dev-server pane and
# mux-inspect consumes its command/layout directly.
dev_repo="$WB_TEST_TMPDIR/code/devrepo"
mkdir -p "$dev_repo"
(cd "$dev_repo" && git init -q)
project_dir="$XDG_CONFIG_HOME/tmux-agent-workbench/projects"
mkdir -p "$project_dir"
{
  printf 'layout=even-vertical\n'
  printf 'dev_command=sleep 30\n'
} > "$project_dir/devrepo.conf"

env -u TMUX WORKBENCH_SESSION=target "$mux_inspect" "$dev_repo" --force >/dev/null
dev_count=$(tmux list-panes -t target:devrepo | wc -l | tr -d ' ')
wb_assert "project config adds a fourth dev pane" test "$dev_count" -eq 4
dev_command=
attempt=0
while [ "$attempt" -lt 3 ]; do
  dev_command=$(tmux display-message -p -t target:devrepo.4 '#{pane_current_command}')
  [ "$dev_command" = sleep ] && break
  sleep 1
  attempt=$((attempt + 1))
done
wb_assert "dev pane runs the configured command" test "$dev_command" = sleep

wb_test_report
