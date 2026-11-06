# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

# Resolve grok's active-sessions registry path. grok records every live
# interactive session in ~/.grok/active_sessions.json as an array of
#   { "session_id": "<uuid>", "pid": <int>, "cwd": "<path>", "opened_at": "..." }
# and updates it on session open/close. GROK_HOME lets tests (and unusual
# installs) point this at a fixture directory.
grok_active_sessions_file() {
	echo "${GROK_HOME:-$HOME/.grok}/active_sessions.json"
}

get_grok_session() {
	local child_pid="$1"
	local args="$2"

	# Method 1 (primary): PID lookup in grok's active-sessions registry.
	# Authoritative for every running session regardless of launch form — a
	# bare `grok` (no args) is recorded here with its session_id — and lookup
	# is keyed by PID, so two sessions sharing a cwd never collide (the
	# cwd-scoped ambiguity that opencode/codex/pi fallbacks suffer from does
	# not apply to grok).
	local registry sid
	registry=$(grok_active_sessions_file)
	if [ -f "$registry" ]; then
		sid=$(jq -r --arg pid "$child_pid" \
			'.[]? | select((.pid | tostring) == $pid) | .session_id // empty' \
			"$registry" 2>/dev/null | head -n1 || true)
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi

	# Method 2 (fallback): -r/--resume <uuid> in process args. Covers the
	# chicken-and-egg window right after a restore, before grok has rewritten
	# active_sessions.json. Anchored to a 36-char UUID so a trailing prompt
	# positional can't be mistaken for the ID. Handles `--resume <id>`,
	# `--resume=<id>`, `-r <id>`, and `-r=<id>`.
	sid=$(echo "$args" | sed -n 's/.*--resume[= ] *\([0-9a-fA-F]\{8\}-[0-9a-fA-F-]\{27\}\).*/\1/p')
	[ -z "$sid" ] && sid=$(echo "$args" | sed -n 's/.*-r[= ] *\([0-9a-fA-F]\{8\}-[0-9a-fA-F-]\{27\}\).*/\1/p')
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi
}
