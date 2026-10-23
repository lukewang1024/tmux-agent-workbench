#!/bin/sh
# Compatibility test entrypoint; current task behavior uses a private tmux server.
set -eu
dir=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd -P)
exec "$dir/tests/python.sh" "$dir/tests/test_task_windows.py" TaskWindows.test_task_cleanup_keeps_dirty_work_and_other_tasks TaskWindows.test_done_can_clean_closed_directions_without_touching_main_checkouts
