#!/usr/bin/env bash
# workbench.tmux — layer 1 (agent-agnostic) TPM entrypoint.
#
# Installed as a single @tpm_plugins entry for the whole repo. This root
# entrypoint explicitly sources git/workbench-git.tmux, so no second config
# stanza is needed. Layer 2 can still opt out with @workbench-disable-git.
set -eu
CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Bridge a tmux option to a global env var, only if the user actually set it —
# leave it alone otherwise so the scripts' own `${VAR:-default}` applies.
bridge_option() {
  val="$(tmux show-option -gqv "$1" 2>/dev/null || true)"
  if [ -n "$val" ]; then
    tmux set-environment -g "$2" "$val"
  fi
}
bridge_option "@workbench-agent" "WORKBENCH_AGENT"
bridge_option "@workbench-git-tool" "WORKBENCH_GIT_TOOL"

# Opinionated defaults remain overrideable through the same tmux options.
if [ -z "$(tmux show-option -gqv @workbench-agent 2>/dev/null || true)" ]; then
  tmux set-option -g @workbench-agent codex
  tmux set-environment -g WORKBENCH_AGENT codex
fi
if [ -z "$(tmux show-option -gqv @workbench-responsive-zoom 2>/dev/null || true)" ]; then
  tmux set-option -g @workbench-responsive-zoom on
fi

. "$CURRENT_DIR/lib/bind-tracked.sh"

# Prepend this plugin's bin/ to the global PATH so anything tmux spawns from
# here on (panes, the agent's own shell-tool calls) can reach these commands
# by bare name — covers every real call site except bootstrapping the very
# first session from a shell tmux never touched (README covers that case).
current_path="$(tmux show-environment -g PATH 2>/dev/null | sed -n 's/^PATH=//p')"
[ -n "$current_path" ] || current_path="$PATH"
case ":$current_path:" in
  *":$CURRENT_DIR/bin:"*) : ;;  # already present (config reload) — don't duplicate
  *) tmux set-environment -g PATH "$CURRENT_DIR/bin:$current_path" ;;
esac

# Publish Workbench's status model through the adaptive theme's generic context
# interface. The theme owns presentation only; all Agent semantics, usage data,
# commands, and mouse behavior remain here. Recursive formats let either plugin
# load first and survive every later theme repaint.
tmux set-option -g @adaptive_context_state '#{@workbench_window_state}'
tmux set-option -g @adaptive_context_label '#{@workbench_window_label}'
tmux set-option -g @adaptive_context_range_open '#[range=user|wb_usage]'
tmux set-option -g @adaptive_context_range_close '#[range=]'
tmux set-option -g @adaptive_session_range_open '#[range=user|wb_prefix]'
tmux set-option -g @adaptive_session_range_close '#[range=]'
tmux set-option -g @adaptive_host_icon '󰍹'
tmux set-option -g @adaptive_host_range_open '#[range=user|wb_host]'
tmux set-option -g @adaptive_host_range_close '#[range=]'
tmux set-option -g @adaptive_action_1_icon ''
tmux set-option -g @adaptive_action_1_range wb_tmux
tmux set-option -g @adaptive_action_2_icon '󰚩'
tmux set-option -g @adaptive_action_2_range wb_agent
tmux set-option -g @adaptive_action_3_icon ''
tmux set-option -g @adaptive_action_3_range wb_sidebar
usage_enabled="$(tmux show-option -gqv @workbench-usage 2>/dev/null || true)"
if [ "$usage_enabled" != off ]; then
  old_usage_source="$(tmux show-option -gqv @agent_usage_source 2>/dev/null || true)"
  usage_source="$(tmux show-option -gqv @workbench-usage-source 2>/dev/null || true)"
  if [ -z "$usage_source" ]; then
    case "$old_usage_source" in codex|claude|trae|opencode) usage_source=$old_usage_source ;; *) usage_source=codex ;; esac
    tmux set-option -g @workbench-usage-source "$usage_source"
  fi
  tmux set-option -g @adaptive_context_suffix \
    "#($CURRENT_DIR/bin/workbench-agent-usage badge #{client_width})"
else
  tmux set-option -gu @adaptive_context_suffix 2>/dev/null || true
fi
tmux set-option -s command-alias[920] \
  "wb-tmux-status-menu=run-shell -b \"'$CURRENT_DIR/bin/workbench-status-popup' tmux '#{client_name}' '#{pane_id}'\""
tmux set-option -s command-alias[921] \
  "wb-agent-status-menu=run-shell -b \"'$CURRENT_DIR/bin/workbench-status-popup' agent '#{client_name}' '#{pane_id}'\""
tmux set-option -s command-alias[922] "wb-sidebar-status=run-shell -b \"$CURRENT_DIR/bin/wb-responsive '#{window_id}'\""
tmux set-option -s command-alias[923] 'wb-other-status=select-window -t ='
status_click_action="if-shell -F '#{==:#{mouse_status_range},wb_prefix}' 'switch-client -T prefix' \"if-shell -F '#{==:#{mouse_status_range},wb_tmux}' 'wb-tmux-status-menu' \\\"if-shell -F '#{==:#{mouse_status_range},wb_agent}' 'wb-agent-status-menu' \\\\\\\"if-shell -F '#{==:#{mouse_status_range},wb_sidebar}' 'wb-sidebar-status' 'wb-other-status'\\\\\\\"\\\"\""
tmux bind-key -T root MouseDown1Status "$status_click_action"
tmux bind-key -T prefix MouseDown1Status "$status_click_action"
workbench_status_ranges='#{||:#{==:#{mouse_status_range},wb_prefix},#{||:#{==:#{mouse_status_range},wb_host},#{||:#{==:#{mouse_status_range},wb_tmux},#{||:#{==:#{mouse_status_range},wb_agent},#{||:#{==:#{mouse_status_range},wb_sidebar},#{==:#{mouse_status_range},wb_usage}}}}}}'
tmux bind-key -T root MouseDown1Status if-shell -F "$workbench_status_ranges" \
  'display-message -p ""' 'select-window -t ='
tmux bind-key -T prefix MouseDown1Status if-shell -F "$workbench_status_ranges" \
  'display-message -p ""' 'select-window -t ='
tmux bind-key -T root MouseUp1Status if-shell -F "$workbench_status_ranges" \
  "run-shell -b \"$CURRENT_DIR/bin/workbench-status-click \\\"#{mouse_status_range}\\\" \\\"#{pane_id}\\\" root \\\"#{client_name}\\\" \\\"#{window_id}\\\"\"" \
  'display-message -p ""'
tmux bind-key -T prefix MouseUp1Status if-shell -F "$workbench_status_ranges" \
  "run-shell -b \"$CURRENT_DIR/bin/workbench-status-click \\\"#{mouse_status_range}\\\" \\\"#{pane_id}\\\" prefix \\\"#{client_name}\\\" \\\"#{window_id}\\\"\"" \
  'display-message -p ""'
tmux set-option -s command-alias[924] \
  "wb-host-status=run-shell -b \"'$CURRENT_DIR/bin/workbench-status-popup' host '#{client_name}' '#{pane_id}'\""
tmux set-option -s command-alias[925] \
  "wb-usage-status=run-shell -b \"sleep 0.15; '$CURRENT_DIR/bin/workbench-agent-usage' menu '#{client_name}'\""
tmux set-option -s command-alias[926] "wb-static-status=$status_click_action"
tmux set-option -s command-alias[927] \
  'wb-status-route=if-shell -F "#{==:#{mouse_status_range},wb_host}" wb-host-status "if-shell -F '\''#{==:#{mouse_status_range},wb_usage}'\'' wb-usage-status wb-static-status"'
tmux bind-key -T root MouseDown1Status wb-status-route
tmux bind-key -T prefix MouseDown1Status if-shell -F \
  '#{==:#{mouse_status_range},wb_prefix}' 'switch-client -T prefix' wb-status-route
tmux unbind-key -T root MouseUp1Status 2>/dev/null || true
tmux bind-key -T prefix MouseUp1Status if-shell -F \
  '#{==:#{mouse_status_range},wb_prefix}' \
  'switch-client -T prefix' 'switch-client -T prefix'
# A Session press moves the client into the prefix table. Its release is then
# decoded in that table; keep it there so touch behaves like a physical prefix.
tmux bind-key -T root MouseDown3Status if-shell -F \
  '#{==:#{mouse_status_range},wb_usage}' \
  "run-shell -b '$CURRENT_DIR/bin/workbench-agent-usage focus'" 'display-message -p ""'

# If the optional theme loaded first, ask it to consume the newly registered
# context values once. Future theme repaints read the same retained options.
adaptive_theme_dir="$(tmux show-option -gqv @adaptive_theme_dir 2>/dev/null || true)"
if [ -n "$adaptive_theme_dir" ] && [ -x "$adaptive_theme_dir/tmux-adaptive-theme.tmux" ]; then
  "$adaptive_theme_dir/tmux-adaptive-theme.tmux"
fi

# Dependency check: mux-inspect-pick hard-requires fzf-tmux. Everything else
# in this layer (mux-agent, mux-inspect) has no external binary dependency.
if ! command -v fzf-tmux >/dev/null 2>&1; then
  tmux display-message "tmux-agent-workbench: fzf-tmux not found — the sidebar inspect picker won't work until it's installed"
fi

bind_tracked "@workbench-key-inspect" "@workbench-_bound-inspect" "" \
  "add repo as inspection window" run-shell -b "$CURRENT_DIR/bin/mux-inspect-pick"
bind_tracked "@workbench-key-project" "@workbench-_bound-project" "" \
  "pick tmuxinator workbench project" run-shell -b "$CURRENT_DIR/bin/workbench-session-pick"

bind_layout() {
  bind_tracked "$1" "$2" "$3" "$4" run-shell -b \
    "$CURRENT_DIR/bin/workbench-layout '$5' '#{pane_id}'"
}
bind_layout @workbench-key-layout-spread @workbench-_bound-layout-spread e "spread panes without resizing sidebar" -E
bind_layout @workbench-key-layout-next @workbench-_bound-layout-next Space "next layout without resizing sidebar" -n
bind_layout @workbench-key-layout-even-horizontal @workbench-_bound-layout-even-horizontal M-1 "even-horizontal without resizing sidebar" even-horizontal
bind_layout @workbench-key-layout-even-vertical @workbench-_bound-layout-even-vertical M-2 "even-vertical without resizing sidebar" even-vertical
bind_layout @workbench-key-layout-main-horizontal @workbench-_bound-layout-main-horizontal M-3 "main-horizontal without resizing sidebar" main-horizontal
bind_layout @workbench-key-layout-main-vertical @workbench-_bound-layout-main-vertical M-4 "main-vertical without resizing sidebar" main-vertical
bind_layout @workbench-key-layout-tiled @workbench-_bound-layout-tiled M-5 "tiled without resizing sidebar" tiled
bind_layout @workbench-key-layout-main-horizontal-mirrored @workbench-_bound-layout-main-horizontal-mirrored M-6 "main-horizontal-mirrored without resizing sidebar" main-horizontal-mirrored
bind_layout @workbench-key-layout-main-vertical-mirrored @workbench-_bound-layout-main-vertical-mirrored M-7 "main-vertical-mirrored without resizing sidebar" main-vertical-mirrored

bind_pane() {
  bind_tracked_repeat "$1" "$2" "$3" "$4" run-shell "$CURRENT_DIR/bin/workbench-select-pane '$5'"
}
bind_pane @workbench-key-pane-left @workbench-_bound-pane-left h "select pane left with responsive zoom" -L
bind_pane @workbench-key-pane-down @workbench-_bound-pane-down j "select pane down with responsive zoom" -D
bind_pane @workbench-key-pane-up @workbench-_bound-pane-up k "select pane up with responsive zoom" -U
bind_pane @workbench-key-pane-right @workbench-_bound-pane-right l "select pane right with responsive zoom" -R
bind_pane @workbench-key-pane-last @workbench-_bound-pane-last o "select next pane with responsive zoom" next
bind_pane @workbench-key-pane-previous @workbench-_bound-pane-previous O "select previous pane with responsive zoom" previous
bind_pane @workbench-key-pane-left-ctrl @workbench-_bound-pane-left-ctrl C-h "select pane left with responsive zoom" -L
bind_pane @workbench-key-pane-down-ctrl @workbench-_bound-pane-down-ctrl C-j "select pane down with responsive zoom" -D
bind_pane @workbench-key-pane-up-ctrl @workbench-_bound-pane-up-ctrl C-k "select pane up with responsive zoom" -U
bind_pane @workbench-key-pane-right-ctrl @workbench-_bound-pane-right-ctrl C-l "select pane right with responsive zoom" -R
bind_pane @workbench-key-pane-left-arrow @workbench-_bound-pane-left-arrow Left "select pane left with responsive zoom" -L
bind_pane @workbench-key-pane-down-arrow @workbench-_bound-pane-down-arrow Down "select pane down with responsive zoom" -D
bind_pane @workbench-key-pane-up-arrow @workbench-_bound-pane-up-arrow Up "select pane up with responsive zoom" -U
bind_pane @workbench-key-pane-right-arrow @workbench-_bound-pane-right-arrow Right "select pane right with responsive zoom" -R

bind_tracked @workbench-key-agent-menu @workbench-_bound-agent-menu M-a "open agent command menu" \
  run-shell "$CURRENT_DIR/bin/workbench-menu agent '#{client_name}'"
bind_tracked @workbench-key-mobile-menu @workbench-_bound-mobile-menu M-t "open compact tmux menu" \
  run-shell "$CURRENT_DIR/bin/workbench-menu tmux '#{client_name}'"

# Attention v2 is an additive Rust subsystem; the existing task/worktree tools
# above remain independent. The public tmux-agent-workbench command is a shell
# dispatcher, so the plugin must resolve the internal Rust core explicitly.
ATTENTION_BIN="${TMUX_AGENT_WORKBENCH_BIN:-}"
if [ -n "$ATTENTION_BIN" ] && [ ! -x "$ATTENTION_BIN" ]; then
  ATTENTION_BIN=""
fi
if [ -z "$ATTENTION_BIN" ] && [ -x "$CURRENT_DIR/target/release/tmux-agent-workbench" ]; then
  ATTENTION_BIN="$CURRENT_DIR/target/release/tmux-agent-workbench"
elif [ -z "$ATTENTION_BIN" ] && [ -x "${XDG_DATA_HOME:-$HOME/.local/share}/tmux-agent-workbench/bin/tmux-agent-workbench-core" ]; then
  ATTENTION_BIN="${XDG_DATA_HOME:-$HOME/.local/share}/tmux-agent-workbench/bin/tmux-agent-workbench-core"
fi

if [ -n "$ATTENTION_BIN" ]; then
  tmux set-option -g @workbench_attention_bin "$ATTENTION_BIN"
  # Sidebars are derived UI, not user panes. Resurrect would otherwise restore
  # each one as an empty shell and then our auto-create hook would add another
  # sidebar beside it. Filter sidebar records and substitute the main-only
  # layout captured by the layout controller.
  tmux set-option -g @resurrect-hook-post-save-layout \
    "'$CURRENT_DIR/bin/workbench-resurrect-save-hook'"
  expected_version="$(sed -n 's/^version *= *"\([^"]*\)"/\1/p' "$CURRENT_DIR/Cargo.toml" | head -1)"
  installed_version="$($ATTENTION_BIN --version 2>/dev/null | awk '{print $2}')"
  if [ -n "$expected_version" ] && [ "$installed_version" != "$expected_version" ]; then
    tmux display-message "tmux-agent-workbench: binary $installed_version differs from repo $expected_version; run ./install"
  fi
  tmux run-shell -b "\"$ATTENTION_BIN\" daemon ensure >/dev/null 2>&1 || tmux display-message 'tmux-agent-workbench: daemon failed to start; run doctor'"
  tmux set-hook -g 'after-new-window[921]' \
    "run-shell -b '\"$ATTENTION_BIN\" daemon wake >/dev/null 2>&1'"
  tmux set-hook -g 'after-split-window[921]' \
    "run-shell -b '\"$ATTENTION_BIN\" daemon wake >/dev/null 2>&1'"
  tmux set-hook -g 'pane-exited[921]' \
    "run-shell -b '\"$ATTENTION_BIN\" daemon wake >/dev/null 2>&1'"

  "$ATTENTION_BIN" sidebar-control configure
  tmux run-shell -b "\"$ATTENTION_BIN\" sidebar-control ensure-all >/dev/null 2>&1"
  current_delay="$(tmux show-option -gqv @sidebar_auto_create_delay 2>/dev/null || true)"
  [ -n "$current_delay" ] || tmux set-option -g @sidebar_auto_create_delay 0

  bind_tracked "@workbench-key-sidebar" "@workbench-_bound-sidebar" "Tab" \
    "toggle Workbench agent sidebar" run-shell \
    "$CURRENT_DIR/bin/wb-responsive '#{window_id}' '#{client_name}'"
  bind_tracked "@workbench-key-sidebar-all" "@workbench-_bound-sidebar-all" "" \
    "toggle Workbench agent sidebars in all windows" run-shell -b \
    "$ATTENTION_BIN sidebar-control toggle-all"

  if [ "$(tmux show-option -gqv @sidebar_auto_create 2>/dev/null || true)" != "off" ]; then
    tmux set-hook -g 'after-new-window[920]' \
      "run-shell -b 'sleep \"#{@sidebar_auto_create_delay}\"; \"$ATTENTION_BIN\" sidebar-control toggle \"#{window_id}\" --create-only'"
  else
    tmux set-hook -gu 'after-new-window[920]' 2>/dev/null || true
  fi
  tmux set-hook -g 'client-attached[920]' \
    "run-shell -b '$CURRENT_DIR/bin/workbench-responsive-maintain \"$ATTENTION_BIN\" \"#{client_name}\"'"
  tmux set-hook -g 'client-resized[920]' \
    "run-shell -b '$CURRENT_DIR/bin/workbench-responsive-maintain \"$ATTENTION_BIN\" \"#{client_name}\"'"
  tmux set-hook -g 'client-session-changed[920]' \
    "run-shell '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
  tmux set-hook -g 'session-window-changed[920]' \
    "run-shell '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
  tmux set-hook -g 'window-resized[920]' \
    "run-shell -b '\"$ATTENTION_BIN\" sidebar-control maintain \"#{hook_window}\"'"
  tmux set-hook -g 'after-kill-pane[920]' \
    "run-shell -b 'sleep 0.05; \"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\" >/dev/null 2>&1'"
  tmux set-hook -g 'pane-exited[920]' \
    "run-shell -b 'sleep 0.05; \"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\" >/dev/null 2>&1'"
  tmux set-hook -gu 'after-resize-pane[920]' 2>/dev/null || true
  tmux bind-key -T root MouseDragEnd1Border run-shell -b \
    "\"$ATTENTION_BIN\" sidebar-control remember \"#{mouse_pane}\""
  tmux set-hook -gu 'client-attached[930]' 2>/dev/null || true
  tmux set-hook -gu 'client-resized[930]' 2>/dev/null || true

  bind_tracked "@workbench-key-session" "@workbench-_bound-session" "" \
    "pick tmux session by agent attention" run-shell -b \
    "$ATTENTION_BIN pick session 2>/dev/null || tmux display-message 'Workbench: session picker unavailable'"
  bind_tracked "@workbench-key-agent" "@workbench-_bound-agent" "" \
    "pick agent" run-shell -b \
    "$ATTENTION_BIN pick agent 2>/dev/null || tmux display-message 'Workbench: Agent picker unavailable'"
  bind_tracked "@workbench-key-attention" "@workbench-_bound-attention" "" \
    "jump to next agent attention" run-shell -b \
    "$ATTENTION_BIN attention next 2>/dev/null || tmux display-message 'Workbench: attention unavailable'"
else
  tmux display-message "tmux-agent-workbench: attention binary missing; run ./install"
fi

# Layer 2 ships with this plugin and self-gates on @workbench-disable-git.
# Source it here so one TPM plugin entry is a complete installation.
if [ -f "$CURRENT_DIR/git/workbench-git.tmux" ]; then
  . "$CURRENT_DIR/git/workbench-git.tmux"
fi
