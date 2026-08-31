#!/bin/sh
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
# shellcheck disable=SC1091 # path is resolved from this script at runtime
. "$dir/helpers.sh"

wb_test_setup "wb-test-install-$$"
trap wb_test_teardown EXIT

repo=$(cd "$dir/.." && pwd -P)
test_home="$WB_TEST_TMPDIR/home"
test_data="$WB_TEST_TMPDIR/data"
test_bin="$WB_TEST_TMPDIR/install-bin"
mkdir -p "$test_home" "$test_data"

HOME=$test_home XDG_DATA_HOME=$test_data "$repo/install" "$test_bin" >"$WB_TEST_TMPDIR/install.out"
wb_assert "installer links canonical tmux-agent-workbench command" test -L "$test_bin/tmux-agent-workbench"
wb_assert "installer retains compatibility wb command" test -L "$test_bin/wb"
wb_assert "installer links mux-handoff into requested bin dir" test -L "$test_bin/mux-handoff"
wb_assert "installed mux-handoff resolves to repo command" test "$(readlink "$test_bin/mux-handoff")" = "$repo/bin/mux-handoff"
wb_assert "installer keeps canonical skill data under XDG" test -L "$test_data/agent/skills/handoff"
wb_assert "installer exposes shared compatibility skill symlink" test -L "$test_home/.agents/skills/handoff"
wb_assert "compatibility skill resolves through XDG to repo skill" test "$(cd "$test_home/.agents/skills/handoff" && pwd -P)" = "$repo/skills/handoff"

canonical_help=$("$test_bin/tmux-agent-workbench" --help 2>&1)
canonical_version=$("$test_bin/tmux-agent-workbench" --version 2>&1)
compat_help_one=$("$test_bin/wb" --help 2>&1)
compat_help_two=$("$test_bin/wb" --help 2>&1)
wb_assert "canonical CLI identifies the tmux namespace" sh -c \
  "printf '%s\\n' \"\$1\" | grep -F 'distinct from the distributed' >/dev/null" sh "$canonical_help"
wb_assert "canonical CLI forwards core commands" sh -c \
  "printf '%s\\n' \"\$1\" | grep -F 'tmux-agent-workbench ' >/dev/null" sh "$canonical_version"
wb_assert "wb short name warns with its exact replacement" sh -c \
  "printf '%s\\n' \"\$1\" | grep -F 'wb is deprecated; use tmux-agent-workbench' >/dev/null" sh "$compat_help_one"
wb_assert "wb short name warns on every invocation" sh -c \
  "printf '%s\\n' \"\$1\" | grep -F 'wb is deprecated; use tmux-agent-workbench' >/dev/null" sh "$compat_help_two"

# Reinstallation must retain the same links without creating backup debris.
HOME=$test_home XDG_DATA_HOME=$test_data "$repo/install" "$test_bin" >"$WB_TEST_TMPDIR/reinstall.out"
wb_assert "reinstall leaves no handoff command backup" test ! -e "$test_bin/mux-handoff~"
wb_assert "reinstall leaves no public command backup" test ! -e "$test_bin/tmux-agent-workbench~"
wb_assert "reinstall leaves no canonical skill backup" test ! -e "$test_data/agent/skills/handoff~"
wb_assert "reinstall leaves no compatibility skill backup" test ! -e "$test_home/.agents/skills/handoff~"

wb_test_report
