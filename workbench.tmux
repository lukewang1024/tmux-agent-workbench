#!/usr/bin/env bash
# workbench.tmux — layer 1 (agent-agnostic) TPM entrypoint.
#
# Installed as a single @tpm_plugins entry for the whole repo; TPM discovers
# and runs every executable *.tmux file it finds under the plugin's cloned
# tree, so this and git/workbench-git.tmux both load from that one entry —
# no second @tpm_plugins line needed. Layer 2 can still opt out entirely (see
# git/workbench-git.tmux's @workbench-disable-git) without a separate install.
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
  tmux display-message "tmux-agent-workbench: fzf-tmux not found — prefix+$(tmux show-option -gqv @workbench-key-inspect 2>/dev/null || echo G) (inspect picker) won't work until it's installed"
fi

bind_tracked "@workbench-key-inspect" "@workbench-_bound-inspect" "G" \
  "add repo as inspection window" run-shell -b "$CURRENT_DIR/bin/mux-inspect-pick"
