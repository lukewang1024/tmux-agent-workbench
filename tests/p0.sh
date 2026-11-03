#!/bin/sh
# Required, fast checks for the workbench's P0 user journeys.
set -eu

repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo"
python_runner="$repo/tests/python.sh"

cargo fmt --check
cargo test --locked --lib --bin tmux-agent-workbench
cargo run --locked --quiet --bin tmux-agent-workbench -- --version | grep -q tmux-agent-workbench
"$python_runner" -B -m unittest discover -s tests -p test_botmux.py
"$python_runner" -B -m unittest discover -s tests -p test_daemon_fixture.py
"$python_runner" -B "$repo/tests/test_task_windows.py" \
  TaskWindows.test_parallel_tasks_have_separate_checkouts_and_agents \
  TaskWindows.test_commands_target_the_calling_task_and_services_do_not_collide \
  TaskWindows.test_grouped_tasks_keep_workspaces_and_cleanup_uses_task_identity
