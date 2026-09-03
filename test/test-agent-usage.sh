#!/bin/sh
set -eu

dir=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
# shellcheck disable=SC1091
. "$dir/helpers.sh"

wb_test_setup "wb-test-agent-usage-$$"
trap wb_test_teardown EXIT

repo=$(CDPATH='' cd -- "$dir/.." && pwd)
tmux new-session -d -s usage
"$repo/bin/workbench-agent-usage" select trae
wb_assert "usage source is stored under the Workbench namespace" \
  test "$(tmux show-option -gqv @workbench-usage-source)" = trae

tmux set-option -g @workbench-usage-source opencode
badge=$("$repo/bin/workbench-agent-usage" badge 120)
wb_assert "provider without a reliable limit hides the badge" test -z "$badge"

wb_test_report
