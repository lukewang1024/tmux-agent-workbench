#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/workbench-install-test.XXXXXX")
installer_cargo_home=${CARGO_HOME:-$HOME/.cargo}
installer_rustup_home=${RUSTUP_HOME:-$HOME/.rustup}

cleanup()
{
  rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

export HOME=$test_root/home
export CARGO_HOME=$installer_cargo_home
export RUSTUP_HOME=$installer_rustup_home
export XDG_DATA_HOME=$test_root/data
export TMUX_AGENT_WORKBENCH_INSTALL_SOURCE=1
mkdir -p "$HOME" "$test_root/bin"

"$repo/install" "$test_root/bin" --no-git >/dev/null
test -x "$test_root/bin/tmux-agent-workbench"
"$test_root/bin/tmux-agent-workbench" --version | grep 'tmux-agent-workbench' >/dev/null
test -L "$test_root/bin/mux-agent"

# A second run must be idempotent and must not create backup files.
"$repo/install" "$test_root/bin" --no-git >/dev/null
test ! -e "$test_root/bin/mux-agent~"

printf '%s\n' 'installer integration: ok'
