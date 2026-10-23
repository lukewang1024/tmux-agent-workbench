#!/bin/sh
# Compatibility test entrypoint; current task behavior uses a private tmux server.
set -eu
dir=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd -P)
exec "$dir/tests/python.sh" "$dir/tests/test_task_windows.py" TaskWindows.test_retired_promote_never_moves_a_live_pane
