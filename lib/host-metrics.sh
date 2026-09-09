#!/bin/sh
# Sourced by workbench.tmux, which provides CURRENT_DIR.
# Keep data and width policy here; the optional theme owns visual presentation.
for metrics_default in \
  '@workbench-host-metrics on' \
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
else
  metrics_full_width=$(tmux show-option -gqv @workbench-host-metrics-full-min-width)
  tmux set-option -g @adaptive_cpu \
    "#{?#{e|>=:#{client_width},$metrics_full_width},#(sh '$CURRENT_DIR/bin/workbench-host-metrics' full),#(sh '$CURRENT_DIR/bin/workbench-host-metrics' cpu)}"
fi
tmux set-option -g @adaptive_cpu_min_width \
  "$(tmux show-option -gqv @workbench-host-metrics-min-width)"
