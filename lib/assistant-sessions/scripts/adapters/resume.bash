# Adapted from tmux-assistant-resurrect, Copyright (c) 2026 Tim Van Wassenhove.
# See LICENSES/tmux-assistant-resurrect-MIT.txt. Sourced by the Workbench save coordinator.

build_resume_command() {
		case "$tool" in
	claude)
		if [ -n "$safe_cli_args" ] || [ -n "$safe_model_arg" ]; then
			resume_cmd="command claude${safe_cli_args}${safe_model_arg} --resume ${safe_sid}"
		else
			resume_cmd="command claude --resume ${safe_sid}"
		fi
		;;
	copilot)
		copilot_cmd="command copilot"
		if [ -n "$copilot_home" ]; then
			env_prefix="${env_prefix}COPILOT_HOME=$(shell_quote "$pane_cmd" "$copilot_home") "
			nu_env="${nu_env}COPILOT_HOME: $(shell_quote "$pane_cmd" "$copilot_home"), "
			# Not masked: this is a state-root path the plugin derived itself,
			# not user-supplied credential material.
			redacted_env_prefix="${redacted_env_prefix}COPILOT_HOME=$(shell_quote "$pane_cmd" "$copilot_home") "
			redacted_nu_env="${redacted_nu_env}COPILOT_HOME: $(shell_quote "$pane_cmd" "$copilot_home"), "
		fi
		if [ -n "$safe_cli_args" ]; then
			resume_cmd="${copilot_cmd}${safe_cli_args} --resume=${safe_sid}"
		else
			resume_cmd="${copilot_cmd} --resume=${safe_sid}"
		fi
		;;
	opencode)
		if [ -n "$safe_cli_args" ]; then
			resume_cmd="command opencode${safe_cli_args} -s ${safe_sid}"
		else
			resume_cmd="command opencode -s ${safe_sid}"
		fi
		;;
	codex)
		if [ -n "$safe_cli_args" ]; then
			resume_cmd="command codex${safe_cli_args} resume ${safe_sid}"
		else
			resume_cmd="command codex resume ${safe_sid}"
		fi
		;;
	pi)
		if [ -n "$safe_cli_args" ]; then
			resume_cmd="command pi${safe_cli_args} --session ${safe_sid}"
		else
			resume_cmd="command pi --session ${safe_sid}"
		fi
		;;
	omp)
		if [ -n "$safe_cli_args" ]; then
			resume_cmd="command omp${safe_cli_args} --resume ${safe_sid}"
		else
			resume_cmd="command omp --resume ${safe_sid}"
		fi
		;;
	grok)
		# Deliberately ignore cli_args for grok. Resuming reloads the
		# session's own model/agent/context from disk, and grok's prompt is a
		# positional argument — replaying captured args risks re-submitting a
		# stale prompt into the resumed session. A clean `grok --resume <id>`
		# is the correct restore. The generic cwd `cd` below still runs first,
		# which also lets grok locate the (cwd-scoped) session directory.
		resume_cmd="command grok --resume ${safe_sid}"
		;;
	*)
		return 1
		;;
	esac
}
