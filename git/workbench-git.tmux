#!/usr/bin/env bash
# workbench-git.tmux — layer 2 (git-worktree workspace) TPM entrypoint.
#
# Depends on layer 1 (../workbench.tmux) for mux-inspect/@workbench_task —
# never duplicates that logic. Loads automatically alongside layer 1 from the
# same single @tpm_plugins entry (TPM discovers every executable *.tmux file
# under the plugin tree). Opt out of this layer entirely — e.g. you only want
# the agent/window model, no opinion about git worktrees — with:
#   set -g @workbench-disable-git 1
set -eu

if [ "$(tmux show-option -gqv @workbench-disable-git 2>/dev/null || true)" = "1" ]; then
  exit 0
fi

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

bridge_option() {
  val="$(tmux show-option -gqv "$1" 2>/dev/null || true)"
  if [ -n "$val" ]; then
    tmux set-environment -g "$2" "$val"
  fi
}
bridge_option "@workbench-code-root" "WORKBENCH_CODE_ROOT"
bridge_option "@workbench-workspace-root" "WORKBENCH_WORKSPACE_ROOT"

. "$CURRENT_DIR/../lib/bind-tracked.sh"

current_path="$(tmux show-environment -g PATH 2>/dev/null | sed -n 's/^PATH=//p')"
[ -n "$current_path" ] || current_path="$PATH"
case ":$current_path:" in
  *":$CURRENT_DIR/bin:"*) : ;;
  *) tmux set-environment -g PATH "$CURRENT_DIR/bin:$current_path" ;;
esac

# M-g pairs with G (G consumes the pool, M-g regenerates it) — untouched by
# the T-family below. T/M-t/M-T are a second, session-flavored family: T picks
# a configured tmuxinator project, M-t starts a NEW workspace/session, and M-T
# derives one from the pane you're already in, no
# prompt. Capital M-T, not M-t twice — and not M-n/M-N, which would collide
# with tmux core's next-window -a / nothing-but-still-confusing-with-core-M-n.
bind_tracked "@workbench-key-regen" "@workbench-_bound-regen" "" \
  "regenerate tmuxinator pool configs" run-shell -b "$CURRENT_DIR/bin/gen-tmuxinator-configs"
bind_tracked "@workbench-key-new" "@workbench-_bound-new" "" \
  "new workspace (feature + repos)" run-shell -b "$CURRENT_DIR/bin/ws-new-prompt"
bind_tracked "@workbench-key-promote" "@workbench-_bound-promote" "" \
  "promote current repo into a new task" run-shell -b "$CURRENT_DIR/bin/ws-promote"
