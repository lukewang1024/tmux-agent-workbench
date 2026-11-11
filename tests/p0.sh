#!/bin/sh
# Required, fast checks for the workbench's P0 user journeys.
set -eu

repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo"
python_runner="$repo/tests/python.sh"

cargo fmt --check
cargo test --locked --lib --bin tmux-agent-workbench
cargo run --locked --quiet --bin tmux-agent-workbench -- --version | grep -q tmux-agent-workbench
"$python_runner" "$repo/tests/macos-spawn-events.py" "$repo/target/debug/tmux-agent-workbench"
if [ "$(uname -s)" = Darwin ]; then
  cargo build --locked --example codex
  "$python_runner" "$repo/tests/low-process-churn.py" "$repo/target/debug/tmux-agent-workbench"
fi
"$python_runner" -B -m unittest discover -s tests -p test_botmux.py
"$python_runner" -B -m unittest discover -s tests -p test_daemon_fixture.py
"$python_runner" -B "$repo/tests/test_task_windows.py" \
  TaskWindows.test_parallel_tasks_have_separate_checkouts_and_agents \
  TaskWindows.test_commands_target_the_calling_task_and_services_do_not_collide \
  TaskWindows.test_grouped_tasks_keep_workspaces_and_cleanup_uses_task_identity \
  TaskWindows.test_task_creation_refreshes_projects_without_duplicate_rebuilds \
  TaskWindows.test_interrupted_batch_refreshes_previous_worktrees \
  TaskWindows.test_project_generation_inventories_each_seed_once
