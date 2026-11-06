# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

get_opencode_session() {
	local child_pid="$1"
	local args="$2"
	local cwd="${3:-}"
	local allow_db_fallback="${4:-1}"

	# Method 1: -s flag in process args (fastest)
	local sid
	sid=$(echo "$args" | sed -n 's/.*-s \(ses_[A-Za-z0-9_]*\).*/\1/p')
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi

	# Method 2: --session flag in process args (supports --session=<id> too)
	sid=$(echo "$args" | sed -n 's/.*--session[= ] *\(ses_[A-Za-z0-9_]*\).*/\1/p')
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi

	# Method 3: plugin state file (handles runtime session switches)
	local state_file="$STATE_DIR/opencode-${child_pid}.json"
	if [ -f "$state_file" ]; then
		sid=$(jq -r '.session_id // empty' "$state_file" 2>/dev/null || true)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi

	# Method 4: SQLite database (version-resilient fallback).
	# OpenCode stores sessions in ~/.local/share/opencode/opencode.db.
	# Query the most recently updated session matching the pane's cwd.
	# Uses python3 (available on Linux and macOS) since sqlite3 CLI
	# is not always installed (e.g. missing on Ubuntu minimal).
	#
	# Limitation: this is NOT PID-specific. If two OpenCode instances run in
	# the same directory (both without -s flags and without plugin state files),
	# both panes get the most recently updated session ID — one of them will be
	# wrong. To avoid this, launch with explicit session IDs: opencode -s <id>.
	local db_file="${HOME}/.local/share/opencode/opencode.db"
	if [ "$allow_db_fallback" = "1" ] && [ -n "$cwd" ] && [ -f "$db_file" ] && command -v python3 >/dev/null 2>&1; then
		sid=$(python3 "$PY_DIR/opencode_db.py" "$db_file" "$cwd" 2>/dev/null || true)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi
}
