#!/bin/sh
# Exercise the imported compatibility contracts without a live tmux or model.
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tests=$repo/tests/assistant-sessions
test_root=$(mktemp -d "${TMPDIR:-/tmp}/workbench-assistant-sessions.XXXXXX")
cleanup() { rm -rf "$test_root"; }
trap cleanup 0 HUP INT TERM
original_path=$PATH
test_bash=${TEST_BASH:-bash}
mkdir -p "$test_root/bin" "$test_root/home" "$test_root/tmp"
cat > "$test_root/bin/tmux" <<'EOF'
#!/bin/sh
exit 1
EOF
chmod +x "$test_root/bin/tmux"
for tool in claude copilot opencode codex pi omp grok traex; do
  cat > "$test_root/bin/$tool" <<'EOF'
#!/bin/sh
exit 0
EOF
  chmod +x "$test_root/bin/$tool"
done
HOME=$test_root/home
XDG_CONFIG_HOME=$test_root/config
XDG_DATA_HOME=$test_root/data
XDG_STATE_HOME=$test_root/state
XDG_CACHE_HOME=$test_root/cache
XDG_RUNTIME_DIR=$test_root/runtime
TMPDIR=$test_root/tmp
PATH=$test_root/bin:$original_path
export HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME XDG_RUNTIME_DIR TMPDIR PATH
unset TMUX TMUX_PANE TMUX_ASSISTANT_RESURRECT_DIR TMUX_RESURRECT_DIR

for suite in copilot-unit-tests grok-unit-tests relaunch-unit-tests \
  restore-unit-tests save-hardening-unit-tests state-dir-unit-tests \
  target-resolution-unit-tests plugin-hardening-unit-tests; do
  printf 'assistant sessions: %s\n' "$suite"
  "$test_bash" "$tests/$suite.sh"
done

if command -v node >/dev/null 2>&1; then
  node "$repo/tests/opencode-v2-session-tracker.mjs" "$repo/lib/opencode-session-track-v2.js"
fi

# This contract creates and destroys its own unique socket and uses no agents.
PATH=$original_path "$test_bash" "$tests/tmux-target-contract-test.sh"
