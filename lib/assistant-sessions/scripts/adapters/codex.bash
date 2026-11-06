# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

get_codex_session() {
	local child_pid="$1"
	local args="$2"
	local cwd="${3:-}"

	# Method 1: session-tags.jsonl (written by Codex at runtime)
	local tags_file="${HOME}/.codex/session-tags.jsonl"
	if [ -f "$tags_file" ]; then
		local sid
		sid=$(grep "\"pid\": *${child_pid}[,}]" "$tags_file" 2>/dev/null |
			tail -1 |
			jq -r '.session // empty' 2>/dev/null || true)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi

	# Method 2: resume arg in process args (chicken-and-egg fallback)
	# After restore, codex is launched as `codex resume <session_id>`.
	local sid
	sid=$(echo "$args" | sed -n "s/.*resume  *\([A-Za-z0-9_-]*\).*/\1/p")
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi

	# Method 3: Codex thread state DB (Codex >= ~0.118 persist state in
	# SQLite: ~/.codex/state_*.sqlite, table `threads`, columns id/cwd/
	# updated_at/archived).  This is the canonical current source — codex
	# writes a `threads` row per session and bumps `updated_at` on every
	# user turn.  A long-lived session that started days ago keeps its
	# same `id` in this table even though no new rollout JSONL is ever
	# written, which is exactly the case Method 4 misses.
	#
	# Strategy: among threads matching our process's cwd that are unarchived
	# and have been updated during this process's lifetime, pick the most
	# recently updated one that isn't already assigned to another pane.
	#
	# The DB file is versioned (state_5.sqlite, bumping on schema changes).
	# We glob for state_*.sqlite inside python3 (avoids `ls -t` pipe and
	# handles spaces in paths cleanly) and pick the newest by mtime.
	if [ -n "$cwd" ] && command -v python3 >/dev/null 2>&1; then
		local process_start
		process_start=$(get_process_start_epoch "$child_pid")
		sid=$(
			USED_CODEX_SESSION_IDS="$USED_CODEX_SESSION_IDS" python3 "$PY_DIR/codex_state_db.py" "$HOME/.codex" "$cwd" "$process_start"
		)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi

	# Method 4: Codex rollout session files (Codex ~0.100-0.117 wrote
	# these; newer versions have moved to SQLite, see Method 3).
	# Releases in that window persisted session metadata under
	# ~/.codex/sessions/*/*.jsonl and included a session_meta record
	# with both id and cwd.
	# We rank candidates by:
	# - matching cwd
	# - preferring session IDs not already assigned during this save
	# - preferring sessions created before the current process start time
	# - preferring sessions closest to the current process start time
	# - preferring recently modified rollout files
	local sessions_root="${HOME}/.codex/sessions"
	if [ -n "$cwd" ] && [ -d "$sessions_root" ] && command -v python3 >/dev/null 2>&1; then
		local process_start
		process_start=$(get_process_start_epoch "$child_pid")
		sid=$(
			USED_CODEX_SESSION_IDS="$USED_CODEX_SESSION_IDS" python3 "$PY_DIR/codex_rollout.py" "$sessions_root" "$cwd" "$process_start"
		)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi
}
