#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
PROVIDER=$ROOT/providers/workbench-tmux-provider

sh -n "$PROVIDER"
test -x "$PROVIDER"

if "$PROVIDER" workspace.inspect 'bad/name' /tmp >/dev/null 2>&1; then
  echo "provider accepted an unsafe tmux name" >&2
  exit 1
fi

printf 'ok - workbench tmux provider syntax and guards\n'

