#!/bin/sh
# test-ws-promote.sh — exercises git/bin/ws-promote: promoting the repo the
# CURRENT PANE happens to sit in should spin up a brand-new session/workspace
# for that repo (agent window + one inspection window, a FRESH worktree/
# branch under WORKBENCH_WORKSPACE_ROOT) — and unlike ws-new, the PANE ITSELF
# moves there via `tmux break-pane`, carrying over whatever was already
# running/displayed in it, rather than a disconnected blank pane appearing
# elsewhere. The original session is expected to disappear once its only
# pane is moved out (standard tmux behavior: a session with zero windows
# doesn't survive).
#
# ws-promote takes no feature-name arg in scenario 1, so it must default the
# new workspace/session name to the repo's own short name.

set -u
. "$(cd "$(dirname "$0")" && pwd)/helpers.sh"

wb_test_setup "wb-test-ws-promote-$$"
trap wb_test_teardown EXIT

# gen-tmuxinator-configs (invoked transitively via ws-add) writes real config
# files under ${XDG_CONFIG_HOME:-$HOME/.config}/tmuxinator by default. Point
# it at a throwaway dir so this test never touches the real user's tmuxinator
# config.
XDG_CONFIG_HOME="$WB_TEST_TMPDIR/xdg-config"
export XDG_CONFIG_HOME
mkdir -p "$XDG_CONFIG_HOME"

bindir=$(cd "$(dirname "$0")/../git/bin" && pwd)

window_named() {
  # window_named <session> <name> — true if that session has a window with
  # exactly that name.
  tmux list-windows -t "$1" -F '#{window_name}' 2>/dev/null | grep -qxF "$2"
}

# -----------------------------------------------------------------------
# 1. Fake repo under CODE_ROOT, an ordinary (non-workspace) session with a
#    pane cd'd into its main checkout, and a MARKER command run in that pane
#    so we can prove the moved pane is the SAME pane (carries its content),
#    not a fresh blank one.
# -----------------------------------------------------------------------
repo=myrepo
maindir="$WORKBENCH_CODE_ROOT/$repo"
mkdir -p "$maindir"
git -C "$maindir" init -q -b main
git -C "$maindir" config user.email test@example.com
git -C "$maindir" config user.name "Test User"
echo hello > "$maindir/file"
git -C "$maindir" add file
git -C "$maindir" commit -q -m init

origsess="wb-test-orig-$$"
tmux new-session -d -s "$origsess" -c "$maindir"
tmux send-keys -t "$origsess" 'echo PROMOTE_MARKER_REPO' Enter
sleep 0.3
panid=$(tmux list-panes -t "$origsess" -F '#{pane_id}')

# -----------------------------------------------------------------------
# 2. Run ws-promote as if from that pane, with no feature-name arg.
# -----------------------------------------------------------------------
out="$WB_TEST_TMPDIR/ws-promote.out"
TMUX_PANE="$panid" TMUX="dummy,0,0" \
  timeout 10 "$bindir/ws-promote" >"$out" 2>&1
# (ignore ws-promote's own exit status — see helpers.sh gotcha #2: its final
# `tmux switch-client` legitimately fails headlessly with "no current
# client")

newsess=$repo
newdest="$WORKBENCH_WORKSPACE_ROOT/$repo/$repo"
newdest_physical=$(cd "$newdest" && pwd -P)

wb_assert "new session '$newsess' now exists" tmux has-session -t "=$newsess"
wb_assert "new session has an agent window" window_named "$newsess" agent
wb_assert "new session has an inspection window for '$repo'" window_named "$newsess" "$repo"
wb_assert "agent window carries the original pane's content (moved, not fresh)" \
  sh -c "tmux capture-pane -t '$newsess:agent' -p | grep -q PROMOTE_MARKER_REPO"

wb_assert "fresh worktree created under WORKBENCH_WORKSPACE_ROOT" test -d "$newdest"
wb_assert "new worktree is a distinct dir from the original checkout" test "$newdest" != "$maindir"
wb_assert "original main checkout still registers the new worktree" \
  sh -c "git -C '$maindir' worktree list | grep -qF '$newdest_physical'"

# The pane's own checkout must not itself have been repointed/rewritten —
# still on its original branch (i.e. it's still `main`'s checkout, not
# silently turned into the new feature worktree).
wb_assert "original checkout still on branch 'main'" \
  sh -c "[ \"\$(git -C '$maindir' rev-parse --abbrev-ref HEAD)\" = main ]"
wb_assert "new worktree got its own branch named '$repo'" \
  sh -c "[ \"\$(git -C '$newdest' rev-parse --abbrev-ref HEAD)\" = '$repo' ]"

# The original SESSION is gone: it had exactly one window/pane, and that
# pane moved out via break-pane — standard tmux behavior destroys a session
# once it has zero windows left. This is the point of "promote": you don't
# end up straddling two places, the work just continues in the new task.
wb_assert "original session '$origsess' is gone (its only pane moved out)" \
  sh -c "! tmux has-session -t '=$origsess' 2>/dev/null"

# -----------------------------------------------------------------------
# 3. A coding task can start from anywhere, not just inside a repo — a pane
#    NOT in any git repo, with an explicit feature name given, must still
#    start a task (zero repos attached, grow it later with ws-add), and the
#    pane itself still moves there (marker check again).
# -----------------------------------------------------------------------
scratchdir="$WB_TEST_TMPDIR/scratch"
mkdir -p "$scratchdir"
scratchsess="wb-test-scratch-$$"
tmux new-session -d -s "$scratchsess" -c "$scratchdir"
tmux send-keys -t "$scratchsess" 'echo PROMOTE_MARKER_SCRATCH' Enter
sleep 0.3
scratchpane=$(tmux list-panes -t "$scratchsess" -F '#{pane_id}')

TMUX_PANE="$scratchpane" TMUX="dummy,0,0" \
  timeout 10 "$bindir/ws-promote" brainstorm >"$WB_TEST_TMPDIR/ws-promote-2.out" 2>&1

wb_assert "no-repo case: session 'brainstorm' now exists" tmux has-session -t "=brainstorm"
wb_assert "no-repo case: only an agent window, no repo folded in" \
  sh -c "[ \"\$(tmux list-windows -t brainstorm | wc -l)\" -eq 1 ]"
wb_assert "no-repo case: agent window carries the original pane's content" \
  sh -c "tmux capture-pane -t brainstorm:agent -p | grep -q PROMOTE_MARKER_SCRATCH"
wb_assert "no-repo case: workspace dir exists but is empty (no repo attached)" \
  sh -c "[ -d '$WORKBENCH_WORKSPACE_ROOT/brainstorm' ] && [ -z \"\$(ls -A "$WORKBENCH_WORKSPACE_ROOT/brainstorm")\" ]"
wb_assert "no-repo case: original scratch session is gone" \
  sh -c "! tmux has-session -t '=$scratchsess' 2>/dev/null"

# -----------------------------------------------------------------------
# 4. Promoting into a name that's already a session must fail cleanly and
#    leave the source pane/session completely untouched — not partially
#    torn down.
# -----------------------------------------------------------------------
collidesess="wb-test-collide-$$"
tmux new-session -d -s "$collidesess" -c "$scratchdir"
collidepane=$(tmux list-panes -t "$collidesess" -F '#{pane_id}')

if TMUX_PANE="$collidepane" TMUX="dummy,0,0" \
    timeout 10 "$bindir/ws-promote" brainstorm >"$WB_TEST_TMPDIR/ws-promote-3.out" 2>&1
then
  collide_rc=0
else
  collide_rc=$?
fi

wb_assert "collision case: ws-promote refuses (nonzero exit)" test "$collide_rc" -ne 0
wb_assert "collision case: source session untouched" tmux has-session -t "=$collidesess"
wb_assert "collision case: source session still has its one window" \
  sh -c "[ \"\$(tmux list-windows -t '$collidesess' | wc -l)\" -eq 1 ]"

wb_test_report
