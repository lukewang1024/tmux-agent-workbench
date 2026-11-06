# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

# --- GitHub Copilot CLI ---
#
# A live Copilot session marks its own state directory with an
# `inuse.<pid>.lock` file (whose content is the same PID):
#
#   $COPILOT_HOME/session-state/<uuid>/inuse.<native-pid>.lock
#
# That is a PID -> session-ID mapping resolvable with one glob: no /proc, no
# lsof, no platform branch, and it works on native Windows too. The lock is
# written at TUI startup, before the first prompt. Copilot can leave the prior
# session's lock behind after an in-process `/resume`, so when one PID has
# multiple valid locks the newest lock is authoritative.
#
# Authenticated sessions may also open a per-session `session.db`, but that is
# not available in the pre-auth contract and mapping it to a PID needs
# platform-specific process inspection. The lock is the portable primary
# contract. test/copilot-contract-test.sh pins it against the real binary.

# COPILOT_HOME replaces the whole ~/.copilot path (same convention as GROK_HOME).
# The deprecated `--config-dir <path>` does the same thing per process and is
# still honored by 1.0.78 -- a session launched with it writes its lock there and
# nothing at all under ~/.copilot -- so it wins when present in the candidate's
# argv. Callers that have no argv handy may omit it.
copilot_session_state_dir() {
	local args="${1:-}"
	local pid="${2:-}"
	local root=""
	local tail=""
	case " $args " in
	*" --config-dir="*) tail="${args##*--config-dir=}" ;;
	*" --config-dir "*) tail="${args##*--config-dir }" ;;
	esac
	if [ -n "$tail" ]; then
		# `ps` has flattened the quoting, so a root containing spaces arrives as
		# several tokens. Prefer the longest prefix that is actually a directory
		# holding a session-state/, then fall back to the first token.
		local candidate="$tail"
		root="${tail%% *}"
		while [ -n "$candidate" ]; do
			if [ -d "$candidate/session-state" ]; then
				root="$candidate"
				break
			fi
			case "$candidate" in
			*" "*) candidate="${candidate% *}" ;;
			*) break ;;
			esac
		done
	fi
	# A tmux hook does not inherit the interactive shell's environment, so a
	# COPILOT_HOME exported from a shell profile is invisible here and the
	# session would be silently skipped. Read it from the Copilot process itself
	# where the kernel allows it (Linux/WSL). macOS cannot read another
	# process's environment unprivileged -- documented, same as capture-env.
	if [ -z "$root" ] && [ -n "$pid" ]; then
		root=$(_copilot_home_from_process "$pid")
	fi
	[ -n "$root" ] || root="${COPILOT_HOME:-$HOME/.copilot}"
	echo "$root/session-state"
}

_copilot_home_from_process() {
	local pid="$1"
	# COPILOT_PROC_ROOT is a test seam so the hermetic suite can exercise this
	# without a live process; production always reads the real /proc.
	local environ_file="${COPILOT_PROC_ROOT:-/proc}/${pid}/environ"
	# Open first: the process may exit between detection and inspection.
	{ exec 3<"$environ_file"; } 2>/dev/null || return 0
	local entry
	while IFS= read -r -d '' entry; do
		case "$entry" in
		COPILOT_HOME=?*)
			printf '%s' "${entry#COPILOT_HOME=}"
			break
			;;
		esac
	done <&3
	exec 3<&-
	return 0
}

# 8-4-4-4-12 hex. Glob-only so it costs no fork: the character class rejects
# anything but hex digits and dashes, and the `?` runs pin the dash positions.
_copilot_is_uuid() {
	[ "${#1}" -eq 36 ] || return 1
	case "$1" in
	*[!0-9a-fA-F-]*) return 1 ;;
	????????-????-????-????-????????????) return 0 ;;
	esac
	return 1
}

# Portable file mtime in epoch seconds: GNU stat, then BSD stat.
_file_mtime_epoch() {
	stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null
}

# Portable high-resolution recency key: GNU stat first, then BSD stat.
# mtime orders normal session switches; ctime breaks ties when two lock mtimes
# land in the same second or are made equal by a filesystem/tooling quirk.
_file_recency_key() {
	LC_ALL=C stat -c '%y|%z' "$1" 2>/dev/null ||
		LC_ALL=C stat -f '%Fm|%Fc' "$1" 2>/dev/null
}

# Is a file that was written at process start older than the process now holding
# that PID? If so it belongs to a dead predecessor whose PID has been recycled,
# and trusting it would map the new process onto the dead session. Advisory: when
# either timestamp is unavailable, report "not stale" rather than lose a real
# session.
_file_predates_process() {
	local file="$1" pid="$2"
	local file_mtime proc_start
	file_mtime=$(_file_mtime_epoch "$file")
	[ -n "$file_mtime" ] || return 1
	proc_start=$(get_process_start_epoch "$pid")
	[ -n "$proc_start" ] || return 1
	# Slack: the macOS start time is derived from second-granular elapsed time.
	[ "$file_mtime" -lt "$((proc_start - 5))" ]
}

# A SIGKILLed Copilot leaves its lock behind. If that PID is later recycled by a
# new Copilot, the stale lock would map the new process onto the dead session.
# The lock is written at session start, so one older than the process claiming
# it is stale.
_copilot_lock_is_live() {
	! _file_predates_process "$1" "$2"
}

get_copilot_session_from_lock() {
	local child_pid="$1"
	local args="${2:-}"
	local state_dir="${3:-}"
	local lock sid recorded lock_key
	local newest_sid="" newest_key="" fallback_sid=""
	[ -n "$state_dir" ] || state_dir=$(copilot_session_state_dir "$args" "$child_pid")
	[ -d "$state_dir" ] || return 0

	for lock in "$state_dir"/*/"inuse.${child_pid}.lock"; do
		[ -f "$lock" ] || continue
		sid="${lock%/*}"
		sid="${sid##*/}"
		_copilot_is_uuid "$sid" || continue
		# Resumability gate. The lock appears at TUI startup, but Copilot only
		# writes session.db (and events.jsonl) once the session has real
		# content, and only such a session can be resumed -- `--resume=<uuid>`
		# on a still-empty one exits with "No session, task, or name matched".
		# Saving it would replay a command that errors in the user's pane, so
		# treat "no session.db yet" as "nothing to save yet".
		[ -f "${lock%/*}/session.db" ] || continue
		# The lock records its owner; a mismatch means we misread the layout.
		recorded=""
		IFS= read -r recorded <"$lock" 2>/dev/null || true
		[ -z "$recorded" ] || [ "$recorded" = "$child_pid" ] || continue
		_copilot_lock_is_live "$lock" "$child_pid" || continue
		[ -n "$fallback_sid" ] || fallback_sid="$sid"
		lock_key=$(_file_recency_key "$lock")
		[ -n "$lock_key" ] || continue
		if [ -z "$newest_key" ] || [[ "$lock_key" > "$newest_key" ]]; then
			newest_key="$lock_key"
			newest_sid="$sid"
		fi
	done
	[ -n "$newest_sid" ] && echo "$newest_sid" || echo "$fallback_sid"
	return 0
}

get_copilot_session() {
	local child_pid="$1"
	local args="$2"
	local allow_args_fallback="${3:-1}"
	local state_dir="${4:-}"
	local sid=""
	[ -n "$state_dir" ] || state_dir=$(copilot_session_state_dir "$args" "$child_pid")

	# Primary: PID-specific, platform-independent, and current after /resume.
	sid=$(get_copilot_session_from_lock "$child_pid" "$args" "$state_dir")
	if [ -n "$sid" ]; then
		echo "$sid"
		return 0
	fi

	# Fallback: explicit session selector in argv. Covers the startup window
	# before the lock exists and setups where the state root is unreadable.
	# Deferred to resolver pass 2 so a stale npm-loader argv cannot beat a live
	# lock held by the native child.
	[ "$allow_args_fallback" = "1" ] || return 0
	local uuid='\([0-9a-fA-F]\{8\}-[0-9a-fA-F]\{4\}-[0-9a-fA-F]\{4\}-[0-9a-fA-F]\{4\}-[0-9a-fA-F]\{12\}\)'
	local flag
	for flag in '--session-id' '--resume' '-r'; do
		sid=$(echo "$args" | sed -n "s/.*${flag}[= ] *$uuid.*/\1/p")
		if [ -n "$sid" ]; then
			# Same resumability gate as the lock path. `copilot --session-id
			# <uuid>` on a blank TUI puts a UUID in argv long before the session
			# can be resumed, so without this the fallback happily saves one
			# that restore can only fail on.
			[ -f "$state_dir/$sid/session.db" ] || continue
			echo "$sid"
			return 0
		fi
	done
	return 0
}
