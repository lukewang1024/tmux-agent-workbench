#!/bin/sh
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

wb_test_setup "wb-test-install-$$"
trap wb_test_teardown EXIT

repo=$(cd "$dir/.." && pwd -P)
test_home="$WB_TEST_TMPDIR/home"
test_data="$WB_TEST_TMPDIR/data"
test_bin="$WB_TEST_TMPDIR/install-bin"
mkdir -p "$test_home" "$test_data"

HOME=$test_home XDG_DATA_HOME=$test_data "$repo/install" "$test_bin" >"$WB_TEST_TMPDIR/install.out"
wb_assert "installer links mux-handoff into requested bin dir" test -L "$test_bin/mux-handoff"
wb_assert "installed mux-handoff resolves to repo command" test "$(readlink "$test_bin/mux-handoff")" = "$repo/bin/mux-handoff"
wb_assert "installer keeps canonical skill data under XDG" test -L "$test_data/agent/skills/handoff"
wb_assert "installer exposes shared compatibility skill symlink" test -L "$test_home/.agents/skills/handoff"
wb_assert "compatibility skill resolves through XDG to repo skill" test "$(cd "$test_home/.agents/skills/handoff" && pwd -P)" = "$repo/skills/handoff"

# Reinstallation must retain the same links without creating backup debris.
HOME=$test_home XDG_DATA_HOME=$test_data "$repo/install" "$test_bin" >"$WB_TEST_TMPDIR/reinstall.out"
wb_assert "reinstall leaves no handoff command backup" test ! -e "$test_bin/mux-handoff~"
wb_assert "reinstall leaves no canonical skill backup" test ! -e "$test_data/agent/skills/handoff~"
wb_assert "reinstall leaves no compatibility skill backup" test ! -e "$test_home/.agents/skills/handoff~"

wb_test_report
