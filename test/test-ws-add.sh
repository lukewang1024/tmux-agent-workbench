#!/bin/sh
# Compatibility test entrypoint; current task behavior uses a private tmux server.
set -eu
dir=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd -P)
exec "$dir/tests/python.sh" "$dir/tests/test_task_windows.py" TaskWindows.test_initializer_and_selected_remote_base_are_preserved TaskWindows.test_legacy_task_and_new_tasks_keep_separate_ownership
