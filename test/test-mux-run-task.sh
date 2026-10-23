#!/bin/sh
# Compatibility test entrypoint; current task behavior uses a private tmux server.
set -eu
dir=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd -P)
exec "$dir/tests/python.sh" "$dir/tests/test_task_windows.py" TaskWindows.test_commands_target_the_calling_task_and_services_do_not_collide TaskWindows.test_instant_service_exit_closes_the_reused_slot
