#!/bin/sh
# The same required checks run locally and in pull-request CI.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo"
python3 -B -m unittest discover -s tests -p test_release_prepare.py
python3 -B -m unittest discover -s tests -p test_botmux.py
cargo fmt --check
cargo test --locked
sh tests/integration-tmux.sh
sh tests/relay-pairing.sh
sh tests/install.sh
