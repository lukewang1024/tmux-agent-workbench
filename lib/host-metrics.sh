#!/bin/sh
# Sourced by workbench.tmux, which provides CURRENT_DIR.
# Keep data and width policy here; the optional theme owns visual presentation.
for metrics_default in \
  '@workbench-host-metrics on' \
  '@workbench-host-metrics-mode compact' \
  '@workbench-host-metrics-min-width 80' \
  '@workbench-host-metrics-full-min-width 140'; do
  metrics_option=${metrics_default% *}
  metrics_value=${metrics_default##* }
  if [ -z "$(tmux show-option -gqv "$metrics_option")" ]; then
    tmux set-option -g "$metrics_option" "$metrics_value"
  fi
done

if [ "$(tmux show-option -gqv @workbench-host-metrics)" = off ]; then
  tmux set-option -g @adaptive_cpu ''
  tmux set-option -g @adaptive_cpu_compact ''
else
  # Old cores keep the standalone status jobs. A low-churn core switches all
  # clients to its shared snapshot once the first complete sample is ready.
  tmux set-option -gq @workbench_status_clock '%s'
  status_age='#{e|-:#{T:@workbench_status_clock},#{@workbench_status_updated_at}}'
  status_fresh="#{&&:#{==:#{@workbench_status_ready},1},#{&&:#{e|>=:$status_age,0},#{e|<:$status_age,15}}}"
  metrics_shell_dir=$(printf '%s' "$CURRENT_DIR" | sed "s/'/'\\\\''/g")
  metrics_cpu_fallback="#(sh '$metrics_shell_dir/bin/workbench-host-metrics' cpu)"
  metrics_fallback="#(sh '$metrics_shell_dir/bin/workbench-host-metrics-status')"
  metrics_cached='#{?#{==:#{@workbench-host-metrics-mode},full},#{@workbench_cached_metrics_full},#{?#{==:#{@workbench-host-metrics-mode},standard},#{@workbench_cached_metrics_standard},#{@workbench_cached_metrics_compact}}}'
  tmux set-option -g @adaptive_cpu_compact \
    "#{?$status_fresh,#{@workbench_cached_cpu},$metrics_cpu_fallback}"
  tmux set-option -g @adaptive_cpu \
    "#{?$status_fresh,$metrics_cached,$metrics_fallback}"
fi
tmux set-option -g @adaptive_cpu_min_width \
  "$(tmux show-option -gqv @workbench-host-metrics-min-width)"
