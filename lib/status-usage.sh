#!/bin/sh
# Sourced by workbench.tmux, which provides CURRENT_DIR.
usage_enabled="$(tmux show-option -gqv @workbench-usage 2>/dev/null || true)"
if [ "$usage_enabled" != off ]; then
  tmux set-option -gq @workbench_status_clock '%s'
  status_age='#{e|-:#{T:@workbench_status_clock},#{@workbench_status_updated_at}}'
  status_fresh="#{&&:#{==:#{@workbench_status_ready},1},#{&&:#{e|>=:$status_age,0},#{e|<:$status_age,15}}}"
  old_usage_source="$(tmux show-option -gqv @agent_usage_source 2>/dev/null || true)"
  usage_source="$(tmux show-option -gqv @workbench-usage-source 2>/dev/null || true)"
  if [ -z "$usage_source" ]; then
    case "$old_usage_source" in codex|claude|trae|opencode) usage_source=$old_usage_source ;; *) usage_source=codex ;; esac
    tmux set-option -g @workbench-usage-source "$usage_source"
  fi
  usage_cached='#{?#{==:#{@workbench-usage-source},claude},#{@workbench_cached_usage_claude},#{?#{==:#{@workbench-usage-source},trae},#{@workbench_cached_usage_trae},#{?#{==:#{@workbench-usage-source},opencode},#{@workbench_cached_usage_opencode},#{@workbench_cached_usage_codex}}}}'
  usage_width='#{?#{m/r:^[0-9]+$,#{@adaptive_compact_min_width}},#{@adaptive_compact_min_width},70}'
  usage_shell_path=$(printf '%s' "$CURRENT_DIR/bin/workbench-agent-usage" | sed "s/'/'\\\\''/g")
  usage_fallback="#('$usage_shell_path' badge #{client_width})"
  tmux set-option -g @adaptive_context_suffix \
    "#{?$status_fresh,#{?#{e|>=:#{client_width},$usage_width},$usage_cached,},$usage_fallback}"
else
  tmux set-option -g @adaptive_context_suffix ''
fi
