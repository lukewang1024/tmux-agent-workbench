# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

_arg_value() {
	local args="$1"
	shift
	# Disable pathname expansion while splitting: an argv value containing a
	# glob char (e.g. `--cwd '*'`) would otherwise expand against the cwd and
	# resolve the wrong session directory. Preserve the caller's noglob state
	# so we never re-enable globbing for a caller that had it off.
	local -a words
	local _had_noglob=0
	case $- in *f*) _had_noglob=1 ;; esac
	set -f
	# shellcheck disable=SC2206  # deliberate word-split of argv string; globbing disabled above
	words=($args)
	[ "$_had_noglob" = 1 ] || set +f
	local i n word flag next
	n=${#words[@]}
	for ((i = 0; i < n; i++)); do
		word="${words[$i]}"
		for flag in "$@"; do
			case "$word" in
			"$flag="*)
				echo "${word#*=}"
				return
				;;
			"$flag")
				next=$((i + 1))
				if [ "$next" -lt "$n" ]; then
					case "${words[$next]}" in
					-*) ;;
					*)
						echo "${words[$next]}"
						return
						;;
					esac
				fi
				;;
			esac
		done
	done
	return 0
}

resolve_path_against() {
	local base="$1"
	local path="$2"
	case "$path" in
	/*)
		echo "$path"
		;;
	*)
		if command -v python3 >/dev/null 2>&1; then
			python3 "$PY_DIR/resolve_path.py" "$base" "$path"
		else
			echo "${base%/}/$path"
		fi
		;;
	esac
}

jsonl_session_id_from_file() {
	local session_file="$1"
	[ -f "$session_file" ] || return 0
	command -v python3 >/dev/null 2>&1 || return 0
	python3 "$PY_DIR/jsonl_header_sid.py" "$session_file"
}

select_jsonl_session_id() {
	local child_pid="$1"
	local cwd="$2"
	local used_ids="$3"
	shift 3
	[ -n "$cwd" ] || return 0
	[ "$#" -gt 0 ] || return 0
	command -v python3 >/dev/null 2>&1 || return 0

	local process_start
	process_start=$(get_process_start_epoch "$child_pid")
	python3 "$PY_DIR/select_jsonl_session.py" "$cwd" "$process_start" "$used_ids" "$@"
}

get_pi_session() {
	local child_pid="$1"
	local args="$2"
	local cwd="${3:-}"

	# Method 1: --session flag in process args (chicken-and-egg fallback)
	# After restore, pi is launched as `pi --session <session_id>`.
	# Supports both `--session <id>` and `--session=<id>` forms.
	local sid
	sid=$(echo "$args" | sed -n 's/.*--session[= ] *\([A-Za-z0-9_-]*\).*/\1/p')
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi

	# Method 2: session files under ~/.pi/agent/sessions/--<cwd>--.
	# Pi writes one JSONL file per session with a session header. The shared
	# selector keeps the existing process-time scoring and same-cwd dedup policy.
	local sessions_root="${PI_CODING_AGENT_SESSION_DIR:-${HOME}/.pi/agent/sessions}"
	if [ -n "$cwd" ] && [ -d "$sessions_root" ]; then
		local safe_cwd session_dir
		safe_cwd=$(echo "$cwd" | sed -e 's#^[\\/]*##' -e 's#[/\\:]#-#g')
		session_dir="${sessions_root}/--${safe_cwd}--"
		sid=$(select_jsonl_session_id "$child_pid" "$cwd" "$USED_PI_SESSION_IDS" "$session_dir")
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi
}

omp_config_root() {
	echo "${PI_CONFIG_DIR:-${HOME}/.omp}"
}

omp_agent_dir() {
	local profile="$1"
	local config_root
	config_root=$(omp_config_root)
	if [ -n "$profile" ]; then
		echo "${config_root}/profiles/${profile}/agent"
	elif [ -n "${PI_CODING_AGENT_DIR:-}" ]; then
		echo "$PI_CODING_AGENT_DIR"
	else
		echo "${config_root}/agent"
	fi
}

omp_session_root() {
	local profile="$1"
	local xdg_data_home="${XDG_DATA_HOME:-${HOME}/.local/share}"
	local config_root
	config_root=$(omp_config_root)
	if [ -n "$profile" ]; then
		if [ -d "${xdg_data_home}/omp/profiles/${profile}" ]; then
			echo "${xdg_data_home}/omp/profiles/${profile}/sessions"
		else
			echo "${config_root}/profiles/${profile}/agent/sessions"
		fi
	elif [ -d "${xdg_data_home}/omp" ]; then
		echo "${xdg_data_home}/omp/sessions"
	elif [ -n "${PI_CODING_AGENT_DIR:-}" ]; then
		echo "${PI_CODING_AGENT_DIR}/sessions"
	else
		echo "${config_root}/agent/sessions"
	fi
}

omp_terminal_session_root() {
	local profile="$1"
	local xdg_state_home="${XDG_STATE_HOME:-${HOME}/.local/state}"
	if [ -n "$profile" ]; then
		if [ -d "${xdg_state_home}/omp/profiles/${profile}" ]; then
			echo "${xdg_state_home}/omp/profiles/${profile}/terminal-sessions"
		else
			echo "$(omp_agent_dir "$profile")/terminal-sessions"
		fi
	elif [ -d "${xdg_state_home}/omp" ]; then
		echo "${xdg_state_home}/omp/terminal-sessions"
	else
		echo "$(omp_agent_dir "")/terminal-sessions"
	fi
}

omp_terminal_id_from_tty() {
	local pane_tty="$1"
	pane_tty="${pane_tty#/dev/}"
	echo "$pane_tty" | sed -e 's#[/\\:]#-#g'
}

omp_sanitize_path_name() {
	echo "$1" | sed -e 's#[/\\:]#-#g'
}

omp_session_dir_names() {
	local cwd="$1"
	local home="${HOME%/}"
	local tmp="${TMPDIR:-/tmp}"
	tmp="${tmp%/}"
	local primary rel legacy
	case "$cwd" in
	"$home")
		primary="-"
		;;
	"$home"/*)
		rel="${cwd#"$home"/}"
		primary="-$(omp_sanitize_path_name "$rel")"
		;;
	"$tmp")
		primary="-tmp"
		;;
	"$tmp"/*)
		rel="${cwd#"$tmp"/}"
		primary="-tmp-$(omp_sanitize_path_name "$rel")"
		;;
	*)
		primary="--$(echo "$cwd" | sed -e 's#^[\\/]*##' -e 's#[/\\:]#-#g')--"
		;;
	esac
	legacy="--$(echo "$cwd" | sed -e 's#^[\\/]*##' -e 's#[/\\:]#-#g')--"
	echo "$primary"
	[ "$legacy" != "$primary" ] && echo "$legacy"
}

get_omp_breadcrumb_session() {
	local pane_tty="$1"
	local lookup_cwd="$2"
	local profile="$3"
	[ -n "$pane_tty" ] || return 0
	[ -n "$lookup_cwd" ] || return 0

	local terminal_id root breadcrumb recorded_cwd session_file sid
	terminal_id=$(omp_terminal_id_from_tty "$pane_tty")
	[ -n "$terminal_id" ] || return 0
	root=$(omp_terminal_session_root "$profile")
	breadcrumb="${root}/${terminal_id}"
	[ -f "$breadcrumb" ] || return 0

	{
		IFS= read -r recorded_cwd || true
		IFS= read -r session_file || true
	} <"$breadcrumb"
	[ "$recorded_cwd" = "$lookup_cwd" ] || return 0
	[ -f "$session_file" ] || return 0

	sid=$(jsonl_session_id_from_file "$session_file")
	if [ -n "$sid" ]; then
		echo "$sid"
	fi
}

get_omp_session() {
	local child_pid="$1"
	local args="$2"
	local pane_cwd="${3:-}"
	local pane_tty="${4:-}"

	local sid
	sid=$(_arg_value "$args" --resume -r --session)
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi

	local lookup_cwd="$pane_cwd"
	local cwd_arg
	cwd_arg=$(_arg_value "$args" --cwd)
	if [ -n "$cwd_arg" ]; then
		lookup_cwd=$(resolve_path_against "$pane_cwd" "$cwd_arg")
	fi

	local profile
	profile=$(_arg_value "$args" --profile)

	sid=$(get_omp_breadcrumb_session "$pane_tty" "$lookup_cwd" "$profile")
	if [ -n "$sid" ]; then
		echo "$sid"
		return
	fi

	local custom_session_dir
	custom_session_dir=$(_arg_value "$args" --session-dir)
	if [ -n "$custom_session_dir" ]; then
		custom_session_dir=$(resolve_path_against "$lookup_cwd" "$custom_session_dir")
		sid=$(select_jsonl_session_id "$child_pid" "$lookup_cwd" "$USED_OMP_SESSION_IDS" "$custom_session_dir")
		if [ -n "$sid" ]; then
			echo "$sid"
			return
		fi
	fi

	local session_root dir_name
	local -a session_dirs=()
	session_root=$(omp_session_root "$profile")
	while IFS= read -r dir_name; do
		[ -n "$dir_name" ] && session_dirs+=("${session_root}/${dir_name}")
	done < <(omp_session_dir_names "$lookup_cwd")
	sid=$(select_jsonl_session_id "$child_pid" "$lookup_cwd" "$USED_OMP_SESSION_IDS" "${session_dirs[@]}")
	if [ -n "$sid" ]; then
		echo "$sid"
	fi
}
