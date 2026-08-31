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

# Dependency check: mux-inspect-pick hard-requires fzf-tmux. Everything else
# in this layer (mux-agent, mux-inspect) has no external binary dependency.
if ! command -v fzf-tmux >/dev/null 2>&1; then
  tmux display-message "tmux-agent-workbench: fzf-tmux not found — the sidebar inspect picker won't work until it's installed"
fi

bind_tracked "@workbench-key-inspect" "@workbench-_bound-inspect" "" \
  "add repo as inspection window" run-shell -b "$CURRENT_DIR/bin/mux-inspect-pick"
bind_tracked "@workbench-key-project" "@workbench-_bound-project" "" \
  "pick tmuxinator workbench project" run-shell -b "$CURRENT_DIR/bin/workbench-session-pick"

# Attention v2 is an additive Rust subsystem; the existing task/worktree tools
# above remain independent. Prefer a release binary installed in the plugin,
# then a local source build, then PATH.
ATTENTION_BIN="${TMUX_AGENT_WORKBENCH_BIN:-}"
if [ -n "$ATTENTION_BIN" ] && [ ! -x "$ATTENTION_BIN" ]; then
  ATTENTION_BIN=""
fi
if [ -z "$ATTENTION_BIN" ] && [ -x "$CURRENT_DIR/bin/tmux-agent-workbench" ]; then
  ATTENTION_BIN="$CURRENT_DIR/bin/tmux-agent-workbench"
elif [ -z "$ATTENTION_BIN" ] && [ -x "$CURRENT_DIR/target/release/tmux-agent-workbench" ]; then
  ATTENTION_BIN="$CURRENT_DIR/target/release/tmux-agent-workbench"
elif [ -z "$ATTENTION_BIN" ] && command -v tmux-agent-workbench >/dev/null 2>&1; then
  ATTENTION_BIN="$(command -v tmux-agent-workbench)"
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

  legacy_sidebar="$(tmux show-option -gqv @agent_sidebar_bin 2>/dev/null || true)"
  if [ -z "$legacy_sidebar" ]; then
    bind_tracked "@workbench-key-sidebar" "@workbench-_bound-sidebar" "Tab" \
      "toggle Workbench agent sidebar" run-shell -b \
      "$CURRENT_DIR/bin/wb-responsive '#{window_id}'"
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
      "run-shell -b '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
    tmux set-hook -g 'client-resized[920]' \
      "run-shell -b '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
    tmux set-hook -g 'client-session-changed[920]' \
      "run-shell '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
    tmux set-hook -g 'session-window-changed[920]' \
      "run-shell '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
    tmux set-hook -g 'window-resized[920]' \
      "run-shell -b '\"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\"'"
    tmux set-hook -g 'after-kill-pane[920]' \
      "run-shell -b 'sleep 0.05; \"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\" >/dev/null 2>&1'"
    tmux set-hook -g 'pane-exited[920]' \
      "run-shell -b 'sleep 0.05; \"$ATTENTION_BIN\" sidebar-control maintain \"#{window_id}\" >/dev/null 2>&1'"
    tmux set-hook -gu 'after-resize-pane[920]' 2>/dev/null || true
    tmux bind-key -T root MouseDragEnd1Border run-shell -b \
      "\"$ATTENTION_BIN\" sidebar-control remember \"#{mouse_pane}\""
  else
    tmux display-message "tmux-agent-workbench: legacy tmux-agent-sidebar is loaded; v2 sidebar disabled (run doctor)"
  fi

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
