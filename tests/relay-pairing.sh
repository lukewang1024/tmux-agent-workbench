#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
binary=$repo/target/debug/tmux-agent-workbench
test_root=$(mktemp -d "${TMPDIR:-/tmp}/workbench-relay-test.XXXXXX")

cleanup()
{
  rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$test_root/bin" "$test_root/local" "$test_root/remote"
export WORKBENCH_TEST_BINARY=$binary
export WORKBENCH_TEST_REMOTE_CONFIG=$test_root/remote
export XDG_CONFIG_HOME=$test_root/local
export XDG_STATE_HOME=$test_root/state
export XDG_CACHE_HOME=$test_root/cache
export XDG_RUNTIME_DIR=$test_root/runtime
export PATH=$test_root/bin:$PATH

cp "$repo/tests/fixtures/fake-ssh" "$test_root/bin/ssh"
chmod 755 "$test_root/bin/ssh"

cargo build --quiet --manifest-path "$repo/Cargo.toml" --bin tmux-agent-workbench

"$binary" relay pair fixture-host >/dev/null
local_store=$test_root/local/tmux-agent-workbench/relay.toml
remote_store=$test_root/remote/tmux-agent-workbench/relay.toml
test -f "$local_store"
test -f "$remote_store"
first_token=$(sed -n 's/^token = "\([0-9a-f]*\)"/\1/p' "$local_store")
[ ${#first_token} -eq 64 ]
grep "token = \"$first_token\"" "$remote_store" >/dev/null

"$binary" relay rotate fixture-host >/dev/null
second_token=$(sed -n 's/^token = "\([0-9a-f]*\)"/\1/p' "$local_store")
[ ${#second_token} -eq 64 ]
[ "$first_token" != "$second_token" ]
grep "token = \"$second_token\"" "$remote_store" >/dev/null

"$binary" relay revoke fixture-host >/dev/null
if grep '^\[\[pairings\]\]' "$local_store" >/dev/null; then
  exit 1
fi
if grep '^\[outbound\]' "$remote_store" >/dev/null; then
  exit 1
fi

printf '%s\n' 'relay pairing integration: ok'
