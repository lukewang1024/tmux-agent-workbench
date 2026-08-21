#!/bin/sh
# test-gen-tmuxinator-configs.sh — every ~/Workspace/* level-1 directory gets
# a pickable config. Standalone repos get one inspection window, multi-repo
# task directories get one per immediate child, and empty tasks get agent-only.
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
generator="$dir/../git/bin/gen-tmuxinator-configs"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/wb-test-gen-XXXXXX")
trap 'rm -rf "$tmp"' EXIT

code="$tmp/code"
workspace="$tmp/workspace"
config="$tmp/config"
mkdir -p "$code" "$workspace" "$config"
export XDG_CONFIG_HOME="$config"

mk_repo() {
  repo=$1
  mkdir -p "$repo"
  git -C "$repo" init -q -b main
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name "Test User"
  : > "$repo/seed"
  git -C "$repo" add seed
  git -C "$repo" commit -q -m seed
}

mk_repo "$workspace/standalone"
mkdir -p "$workspace/empty"
mkdir -p "$workspace/multi"
mk_repo "$workspace/multi/alpha"
mk_repo "$workspace/multi/beta"

"$generator" "$code" "$workspace" >/dev/null

assert_file() { test -f "$1" || { echo "missing: $1" >&2; exit 1; }; }
assert_line() { grep -qxF "$2" "$1" || { echo "missing line '$2' in $1" >&2; exit 1; }; }
assert_no_line() { ! grep -qxF "$2" "$1" || { echo "unexpected line '$2' in $1" >&2; exit 1; }; }

standalone="$config/tmuxinator/standalone.yml"
multi="$config/tmuxinator/multi.yml"
empty="$config/tmuxinator/empty.yml"
assert_file "$standalone"
assert_file "$multi"
assert_file "$empty"

assert_line "$standalone" "root: $workspace/standalone"
assert_line "$standalone" "  - agent:"
assert_line "$standalone" "  - standalone:"

assert_line "$multi" "root: $workspace/multi/alpha"
assert_line "$multi" "  - agent:"
assert_line "$multi" "  - alpha:"
assert_line "$multi" "  - beta:"

assert_line "$empty" "root: $workspace/empty"
assert_line "$empty" "  - agent:"
assert_no_line "$empty" "  - empty:"

echo "PASS: Workspace level-1 directory configs include agent and relevant inspection windows"
