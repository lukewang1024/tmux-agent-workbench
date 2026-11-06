#!/usr/bin/env bash
# Claude Code SessionEnd hook — removes the session tracking state file.
# Receives JSON on stdin with session_id, cwd, etc.
#
# Install: add to ~/.claude/settings.json under hooks.SessionEnd

set -euo pipefail

# Source shared find_claude_pid() helper
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-claude-pid.sh
source "$HOOK_DIR/lib-claude-pid.sh"
# assistant_state_dir() — must resolve to the same path the SessionStart hook
# wrote to, and the same one the save hook reads.
# shellcheck source=../scripts/lib-detect.sh
source "$HOOK_DIR/../scripts/lib-detect.sh"

STATE_DIR="$(assistant_state_dir)"

CLAUDE_PID=$(find_claude_pid)
rm -f "$STATE_DIR/claude-$CLAUDE_PID.json" 2>/dev/null || true
