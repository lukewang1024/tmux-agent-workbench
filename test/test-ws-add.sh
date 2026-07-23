#!/bin/sh
# test-ws-add.sh — the scenario this whole layer exists for: a task that
# starts with NO upfront knowledge of which repo it needs.
#
# A workspace session is already in progress (built by hand here, NOT via
# ws-new) with zero member repos, marked as a task workbench the same way
# ws-new marks one (@workbench_task=1). From WITHIN that session's own
# context — TMUX pointed at it, WORKBENCH_FEATURE deliberately left unset —
# ws-add must auto-detect the feature name from the current tmux session via
# `tmux display-message -p '#S'`, exactly like a real agent running inside
# that session's window would experience it. It must then create the repo's
# worktree and its inspection window, and calling it again for the same repo
# must be a no-op: no duplicate worktree, no duplicate window, no error.
#
# set -u only (not -e): several of the commands below are asserted on their
# own exit code (the ws-add invocations, in particular), and `var=$(cmd)`
# under errexit aborts the whole script the instant `cmd` exits non-zero —
# before wb_assert ever gets a chance to record it as a FAIL. See helpers.sh
# gotcha #2 for the same class of issue elsewhere in this test layer.
set -u
. "$(cd "$(dirname "$0")" && pwd)/helpers.sh"

wb_test_setup "wb-test-ws-add-$$"
trap wb_test_teardown EXIT

# ws-add internally calls gen-tmuxinator-configs, which writes real
# tmuxinator configs to $XDG_CONFIG_HOME/tmuxinator (default ~/.config) —
# sandbox that too so this test never touches the real machine's config.
XDG_CONFIG_HOME="$WB_TEST_TMPDIR/config"
export XDG_CONFIG_HOME

WS_ADD=$(cd "$(dirname "$0")/../git/bin" && pwd)/ws-add

feature="in-progress-ws"
repo="somerepo"

# ---------------------------------------------------------------------------
# 1. A fake repo under CODE_ROOT, with at least one real commit — ws-add's
#    no-existing-branch fallback path points the new worktree's branch at
#    the main checkout's current HEAD, which needs to actually resolve.
# ---------------------------------------------------------------------------
repodir="$WORKBENCH_CODE_ROOT/$repo"
mkdir -p "$repodir"
git -C "$repodir" init -q
git -C "$repodir" config user.email "test@example.com"
git -C "$repodir" config user.name "Test"
echo "hello" > "$repodir/file.txt"
git -C "$repodir" add file.txt
git -C "$repodir" commit -q -m "initial"

# ---------------------------------------------------------------------------
# 2. A bare workspace session — built by hand, NOT via ws-new — representing
#    a workspace already in progress with zero member repos. Only the
#    @workbench_task marker is set, exactly what ws-new itself would set.
# ---------------------------------------------------------------------------
mkdir -p "$WORKBENCH_WORKSPACE_ROOT/$feature"
tmux new-session -d -s "$feature" -c "$WORKBENCH_WORKSPACE_ROOT/$feature" -n agent
tmux set-option -t "$feature" @workbench_task 1

# ---------------------------------------------------------------------------
# 3. Enter that session's own context: point TMUX at it, the way a real
#    attached client's environment would, WITHOUT setting WORKBENCH_FEATURE —
#    ws-add must auto-detect the feature name via `tmux display-message`.
# ---------------------------------------------------------------------------
sockpath=$(tmux display-message -p '#{socket_path}')
pid=$(tmux display-message -p '#{pid}')
sessid=$(tmux display-message -t "$feature" -p '#{session_id}')
TMUX="$sockpath,$pid,${sessid#\$}"
export TMUX
unset WORKBENCH_FEATURE

dest="$WORKBENCH_WORKSPACE_ROOT/$feature/$repo"

# ---------------------------------------------------------------------------
# 4. First call: creates the worktree + inspection window.
# ---------------------------------------------------------------------------
out1=$("$WS_ADD" "$repo" 2>&1); rc1=$?
printf '%s\n' "$out1"
wb_assert "first ws-add call exits 0" [ "$rc1" -eq 0 ]
wb_assert "worktree created under WORKBENCH_WORKSPACE_ROOT/$feature/$repo" test -d "$dest"
wb_assert "worktree checked out real repo content" test -f "$dest/file.txt"
wb_assert "inspection window '$repo' appeared" \
  sh -c "tmux list-windows -t '$feature' -F '#W' | grep -qxF '$repo'"

wincount1=$(tmux list-windows -t "$feature" -F '#W' | grep -cxF "$repo")
wtcount1=$(git -C "$repodir" worktree list | wc -l)

# ---------------------------------------------------------------------------
# 5. Second call, same repo, same session context: must be idempotent — no
#    duplicate-worktree error, no duplicate window.
# ---------------------------------------------------------------------------
out2=$("$WS_ADD" "$repo" 2>&1); rc2=$?
printf '%s\n' "$out2"
wb_assert "second ws-add call exits 0 (idempotent)" [ "$rc2" -eq 0 ]

wincount2=$(tmux list-windows -t "$feature" -F '#W' | grep -cxF "$repo")
wtcount2=$(git -C "$repodir" worktree list | wc -l)

wb_assert "no duplicate worktree registered" [ "$wtcount2" -eq "$wtcount1" ]
wb_assert "still exactly one inspection window" [ "$wincount2" -eq 1 ]
wb_assert "worktree still present after second call" test -d "$dest"

wb_test_report
