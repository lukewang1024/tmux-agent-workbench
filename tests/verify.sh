#!/bin/sh
# The same required checks run locally and in pull-request CI.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo"
python_runner="$repo/tests/python.sh"
"$python_runner" -B -m unittest discover -s tests -p test_release_prepare.py
"$python_runner" -B -m unittest discover -s tests -p test_botmux.py
"$python_runner" -B -m unittest discover -s tests -p test_daemon_fixture.py
cargo fmt --check
cargo test --locked
sh tests/integration-tmux.sh
sh tests/relay-pairing.sh
sh tests/install.sh
