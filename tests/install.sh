#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
sh "$repo/tests/core-selection.sh"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/workbench-install-test.XXXXXX")
installer_runtime_root=$(mktemp -d /tmp/wb-install-run.XXXXXX)
installer_cargo_home=${CARGO_HOME:-$HOME/.cargo}
installer_rustup_home=${RUSTUP_HOME:-$HOME/.rustup}

cleanup()
{
  rm -rf "$test_root"
  rm -rf "$installer_runtime_root"
}
trap cleanup EXIT HUP INT TERM

export HOME=$test_root/home
export CARGO_HOME=$installer_cargo_home
export RUSTUP_HOME=$installer_rustup_home
export XDG_DATA_HOME=$test_root/data
export XDG_CONFIG_HOME=$test_root/config
export XDG_STATE_HOME=$test_root/state
export XDG_CACHE_HOME=$test_root/cache
export XDG_RUNTIME_DIR=$installer_runtime_root
unset TMUX TMUX_PANE TMUX_AGENT_WORKBENCH_TMUX_SOCKET TMUX_AGENT_WORKBENCH_BIN
export TMUX_AGENT_WORKBENCH_INSTALL_SOURCE=1
mkdir -p "$HOME" "$test_root/bin"

"$repo/install" "$test_root/bin" --no-git >/dev/null
test -x "$test_root/bin/tmux-agent-workbench"
"$test_root/bin/tmux-agent-workbench" --version | grep 'tmux-agent-workbench' >/dev/null
test -L "$test_root/bin/mux-agent"
# Agent instructions stay owned by the plugin and are readable before tmux starts.
"$test_root/bin/tmux-agent-workbench" conventions > "$test_root/conventions.md"
cmp "$repo/agent-conventions/AGENTS.snippet.md" "$test_root/conventions.md"
test -f "$HOME/.agents/skills/reorganise-agents/SKILL.md"
"$test_root/bin/tmux-agent-workbench" reorganise --help >/dev/null
"$test_root/bin/tmux-agent-workbench" reorganize --help >/dev/null
"$test_root/bin/tmux-agent-workbench" title set --help | grep -- '--session <SESSION>' >/dev/null
if "$test_root/bin/tmux-agent-workbench" conventions unexpected >/dev/null 2>&1; then
  exit 1
fi
"$test_root/bin/wb" config check >/dev/null

# A second run must be idempotent and must not create backup files.
"$repo/install" "$test_root/bin" --no-git >/dev/null
test ! -e "$test_root/bin/mux-agent~"
for installer_socket in "$installer_runtime_root"/tmux-agent-workbench/daemon-*.sock; do
  test ! -e "$installer_socket"
done

# Release-only mode may deliberately download a pre-handoff release. Its
# installer must retain compatibility with the older daemon command surface.
legacy_plugin="$test_root/legacy-plugin"
legacy_tools="$test_root/legacy-tools"
legacy_archive="$test_root/legacy-archive"
mkdir -p "$legacy_plugin" "$legacy_tools" "$legacy_archive"
cp "$repo/install" "$repo/Cargo.toml" "$legacy_plugin/"
cat > "$legacy_archive/tmux-agent-workbench" <<'SH'
#!/bin/sh
case "$*" in
  --version) printf '%s\n' 'tmux-agent-workbench 2.0.0-beta.35' ;;
  'daemon ensure --help') printf '%s\n' 'Usage: tmux-agent-workbench daemon ensure' ;;
  *) exit 2 ;;
esac
SH
chmod +x "$legacy_archive/tmux-agent-workbench"
tar -czf "$test_root/legacy-release.tar.gz" -C "$legacy_archive" tmux-agent-workbench
cat > "$legacy_tools/curl" <<'SH'
#!/bin/sh
cp "$WORKBENCH_TEST_RELEASE_ARCHIVE" "$4"
SH
cat > "$legacy_tools/codesign" <<'SH'
#!/bin/sh
exit 0
SH
chmod +x "$legacy_tools/curl" "$legacy_tools/codesign"
PATH="$legacy_tools:$PATH" WORKBENCH_TEST_RELEASE_ARCHIVE="$test_root/legacy-release.tar.gz" \
  TMUX_AGENT_WORKBENCH_RELEASE_ONLY=1 "$legacy_plugin/install" "$test_root/legacy-bin" --no-git >/dev/null
"$XDG_DATA_HOME/tmux-agent-workbench/bin/tmux-agent-workbench-core" --version | \
  grep '2.0.0-beta.35' >/dev/null

# An iSH source checkout must select a static 32-bit release without Cargo.
# Use fresh data so an installed host binary cannot hide a wrong target.
ish_plugin="$test_root/ish-plugin"
ish_tools="$test_root/ish-tools"
ish_archive="$test_root/ish-archive"
mkdir -p "$ish_plugin" "$ish_tools" "$ish_archive"
cp "$repo/install" "$repo/Cargo.toml" "$ish_plugin/"
ish_version=$(sed -n 's/^version *= *"\([^"]*\)"/\1/p' "$repo/Cargo.toml" | head -1)
cat > "$ish_archive/tmux-agent-workbench" <<SH
#!/bin/sh
case \$* in
  --version) printf '%s\n' 'tmux-agent-workbench $ish_version' ;;
  'daemon ensure --help') printf '%s\n' 'Usage: tmux-agent-workbench daemon ensure' ;;
  *) exit 2 ;;
esac
SH
chmod +x "$ish_archive/tmux-agent-workbench"
tar -czf "$test_root/ish-release.tar.gz" -C "$ish_archive" tmux-agent-workbench
cat > "$ish_tools/uname" <<'SH'
#!/bin/sh
case $1 in
  -s) printf '%s\n' Linux ;;
  -m) printf '%s\n' i686 ;;
  -o) printf '%s\n' GNU/Linux ;;
  *) exit 2 ;;
esac
SH
cat > "$ish_tools/curl" <<'SH'
#!/bin/sh
case $2 in
  */releases/download/v*/tmux-agent-workbench-i586-unknown-linux-musl.tar.gz) ;;
  *) printf 'unexpected iSH release URL: %s\n' "$2" >&2; exit 1 ;;
esac
cp "$WORKBENCH_TEST_RELEASE_ARCHIVE" "$4"
SH
cat > "$ish_tools/cargo" <<'SH'
#!/bin/sh
printf '%s\n' 'iSH installation attempted a native Cargo build' >&2
exit 1
SH
chmod +x "$ish_tools/uname" "$ish_tools/curl" "$ish_tools/cargo"
(
  unset TERMUX_VERSION PREFIX TMUX_AGENT_WORKBENCH_INSTALL_SOURCE
  export XDG_DATA_HOME="$test_root/ish-data"
  export PATH="$ish_tools:$PATH"
  export WORKBENCH_TEST_RELEASE_ARCHIVE="$test_root/ish-release.tar.gz"
  "$ish_plugin/install" "$test_root/ish-bin" --no-git >/dev/null
  "$XDG_DATA_HOME/tmux-agent-workbench/bin/tmux-agent-workbench-core" --version | grep "$ish_version" >/dev/null
  # The updater passes its selected prerelease; GitHub Latest can lack iSH.
  export TMUX_AGENT_WORKBENCH_RELEASE_ONLY=1
  export TMUX_AGENT_WORKBENCH_RELEASE_TAG="v$ish_version"
  "$ish_plugin/install" "$test_root/ish-bin" --no-git >/dev/null
  "$XDG_DATA_HOME/tmux-agent-workbench/bin/tmux-agent-workbench-core" --version | grep "$ish_version" >/dev/null
)

printf '%s\n' 'installer integration: ok'
