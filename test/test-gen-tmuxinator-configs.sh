#!/bin/sh
# Compatibility test entrypoint; current task behavior uses a private tmux server.
set -eu
dir=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd -P)
exec "$dir/tests/python.sh" "$dir/tests/test_task_windows.py" TaskWindows.test_generator_restores_only_task_windows TaskWindows.test_code_pool_launcher_creates_an_isolated_task
