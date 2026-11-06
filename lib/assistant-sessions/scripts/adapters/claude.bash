# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

# --- Session ID extraction ---

get_claude_session() {
	local claude_pid="$1"
	local args="$2"

	# Method 1: SessionStart hook state file (keyed by Claude PID).
	# The hook walks up the process tree to find the main 'claude' process,
	# so the state file is named claude-{claude_pid}.json.
	local state_file="$STATE_DIR/claude-${claude_pid}.json"
	if [ -f "$state_file" ]; then
		local sid
		sid=$(jq -r '.session_id // empty' "$state_file" 2>/dev/null || true)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi

	# Method 2: session selector in process args (chicken-and-egg fallback).
	# After restore, claude is launched as `claude --resume <session_id>`, and
	# `claude --session-id <uuid>` names the session up front the same way.
	# If the SessionStart hook hasn't fired yet, the ID is still in the args.
	#
	# _arg_value is the same helper get_omp_session already uses. It handles
	# both `--flag <id>` and `--flag=<id>`, and it will not mistake a following
	# option for the value -- the old regex read `claude --resume --model opus`
	# as the session ID `--model`. Two calls rather than one so --resume keeps
	# precedence when both flags are present, as it did before.
	local sid
	sid=$(_arg_value "$args" --resume)
	[ -n "$sid" ] || sid=$(_arg_value "$args" --session-id)
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi
}
