#!/bin/sh
# test-ws-help.sh — all public ws-* commands provide side-effect-free help and
# reject unknown options instead of treating them as workspace/repo names.
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

wb_test_setup "wb-test-ws-help-$$"
trap wb_test_teardown EXIT

bindir=$(cd "$dir/../git/bin" && pwd)

for name in ws-new ws-add ws-done ws-promote ws-new-prompt; do
  cmd="$bindir/$name"
  for flag in -h --help; do
    out="$WB_TEST_TMPDIR/$name-${flag#-}.out"
    if "$cmd" "$flag" >"$out" 2>&1; then
      rc=0
    else
      rc=$?
    fi
    wb_assert "$name $flag exits 0" test "$rc" -eq 0
    wb_assert "$name $flag prints usage" grep -q "^usage: $name" "$out"
  done

  out="$WB_TEST_TMPDIR/$name-unknown.out"
  if "$cmd" --definitely-unknown >"$out" 2>&1; then
    rc=0
  else
    rc=$?
  fi
  wb_assert "$name rejects an unknown option with exit 2" test "$rc" -eq 2
  wb_assert "$name names the unknown option" grep -q "unknown option: --definitely-unknown" "$out"
done

wb_assert "ws-new --help creates no workspace named --help" \
  test ! -e "$WORKBENCH_WORKSPACE_ROOT/--help"
wb_assert "ws-new --help creates no tmux session named --help" \
  sh -c "! tmux has-session -t '=--help' 2>/dev/null"

wb_test_report
