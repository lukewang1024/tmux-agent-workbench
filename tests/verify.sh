#!/bin/sh
# The same required checks run locally and in pull-request CI.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo"
python_runner="$repo/tests/python.sh"
"$python_runner" -B -m unittest discover -s tests -p test_release_prepare.py
"$python_runner" -B -m unittest discover -s tests -p test_botmux.py
"$python_runner" -B -m unittest discover -s tests -p test_daemon_fixture.py
"$python_runner" -B -m unittest discover -s tests -p test_host_metrics.py
"$python_runner" -B -m unittest discover -s tests -p test_reorganise.py
"$python_runner" -B -m unittest discover -s tests -p test_task_windows.py
sh test/test-resolve-session.sh
if command -v bash >/dev/null 2>&1; then
  bash --posix test/test-resolve-session.sh
fi
"$python_runner" -B -m unittest discover -s test -p 'test_host_metrics*.py'
"$python_runner" tests/status-prefix-mouse.py
cargo fmt --check
cargo test --locked
"$python_runner" tests/client-daemon-timeouts.py
"$python_runner" tests/client-notifications.py
"$python_runner" tests/client-resume.py target/debug/tmux-agent-workbench
"$python_runner" tests/client-window-resize.py target/debug/tmux-agent-workbench
sh tests/integration-tmux.sh
sh tests/relay-pairing.sh
sh tests/install.sh
