#!/bin/sh
# test-ws-done.sh — exercises ws-done's dirty/clean worktree policy:
#   1. without --force: clean worktrees are removed, dirty ones are KEPT
#      (git worktree remove refuses them), the tmux session is always killed,
#      and the workspace dir itself survives as long as a dirty member
#      remains in it.
#   2. with --force: the dirty worktree is discarded too, and the now-empty
#      workspace dir is removed.
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

repo_root=$(cd "$dir/.." && pwd)
WS_DONE="$repo_root/git/bin/ws-done"

wb_test_setup "wb-test-ws-done-$$"
trap wb_test_teardown EXIT

# Isolate gen-tmuxinator-configs's writes (ws-done shells out to it at the
# end) away from the real user's ~/.config/tmuxinator.
export XDG_CONFIG_HOME="$WB_TEST_TMPDIR/xdg-config"
mkdir -p "$XDG_CONFIG_HOME"

feature="myfeature"
wsdir="$WORKBENCH_WORKSPACE_ROOT/$feature"

# --- helpers used only by this test's assertions ---------------------------
no_tmux_session() { ! tmux has-session -t "=$1" 2>/dev/null; }
is_dir()          { [ -d "$1" ]; }
not_dir()         { [ ! -d "$1" ]; }
contains()        { grep -q "$1" "$2"; }

# --- set up two fake repos under WORKBENCH_CODE_ROOT ------------------------
repo_clean="$WORKBENCH_CODE_ROOT/repo-clean"
repo_dirty="$WORKBENCH_CODE_ROOT/repo-dirty"
for r in "$repo_clean" "$repo_dirty"; do
  mkdir -p "$r"
  git -C "$r" init -q
  git -C "$r" config user.email test@example.com
  git -C "$r" config user.name "Test User"
  echo hello > "$r/file.txt"
  git -C "$r" add file.txt
  git -C "$r" commit -q -m init
done

# --- manually create worktrees for both repos under the workspace dir ------
mkdir -p "$wsdir"
branch_clean=$(git -C "$repo_clean" symbolic-ref --short HEAD)
branch_dirty=$(git -C "$repo_dirty" symbolic-ref --short HEAD)
git -C "$repo_clean" worktree add -q "$wsdir/repo-clean" -b "$feature" "$branch_clean"
git -C "$repo_dirty" worktree add -q "$wsdir/repo-dirty" -b "$feature" "$branch_dirty"

# make one of the two worktrees dirty (uncommitted change to a tracked file)
echo "uncommitted change" >> "$wsdir/repo-dirty/file.txt"

# --- a tmux session for the workspace ---------------------------------------
tmux new-session -d -s "$feature" -c "$wsdir" -n agent

wb_assert "session exists before ws-done" tmux has-session -t "=$feature"
wb_assert "clean worktree exists before ws-done" is_dir "$wsdir/repo-clean"
wb_assert "dirty worktree exists before ws-done" is_dir "$wsdir/repo-dirty"

# =============================================================================
# Step 2: ws-done <feature> (no --force)
# =============================================================================
out1="$WB_TEST_TMPDIR/ws-done.out"
"$WS_DONE" "$feature" >"$out1" 2>&1 || true

wb_assert "tmux session gone after ws-done" no_tmux_session "$feature"
wb_assert "clean worktree removed" not_dir "$wsdir/repo-clean"
wb_assert "dirty worktree KEPT (git worktree remove refused it)" is_dir "$wsdir/repo-dirty"
wb_assert "ws-done reports the dirty worktree as KEPT" contains "KEPT" "$out1"
wb_assert "workspace dir still exists (dirty member remains)" is_dir "$wsdir"

# =============================================================================
# Step 3: ws-done --force <feature>
# =============================================================================
out2="$WB_TEST_TMPDIR/ws-done-force.out"
"$WS_DONE" --force "$feature" >"$out2" 2>&1 || true

wb_assert "dirty worktree removed after --force" not_dir "$wsdir/repo-dirty"
wb_assert "workspace dir removed after --force" not_dir "$wsdir"

wb_test_report
