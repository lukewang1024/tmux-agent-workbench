#!/bin/sh
# test-ws-new.sh — exercises git/bin/ws-new:
#   1. feature name, ZERO repos: a session named after the feature springs up
#      with exactly one "agent" window, and the workspace dir under
#      WORKBENCH_WORKSPACE_ROOT exists but stays empty (no repos to fold in).
#   2. feature name + TWO repos (one plain name, one repo:branch): both
#      worktrees materialize under WORKBENCH_WORKSPACE_ROOT/<feature>/<repo>
#      on the right branches, the fake CODE_ROOT main checkouts are left
#      untouched, and the session ends up with the agent window plus one
#      3-pane inspection window per repo.
#
# Both invocations hit ws-new's final `tmux attach`/`switch-client` step,
# which legitimately fails headless with "no current client" (helpers.sh
# gotcha #2) — wrapped in `timeout 10` and its own exit status is ignored;
# assertions check resulting tmux/git state instead.
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

WSNEW=$(cd "$dir/../git/bin" && pwd)/ws-new

wb_test_setup "wb-test-ws-new-$$"
trap wb_test_teardown EXIT

# gen-tmuxinator-configs (invoked transitively via ws-add, scenario 2) writes
# real config files under ${XDG_CONFIG_HOME:-$HOME/.config}/tmuxinator by
# default. Point it at a throwaway dir so this test never touches the real
# user's tmuxinator config.
XDG_CONFIG_HOME="$WB_TEST_TMPDIR/xdg-config"
export XDG_CONFIG_HOME
mkdir -p "$XDG_CONFIG_HOME"

# mux-agent execs $WORKBENCH_AGENT (claude/codex/opencode) inside the new
# "agent" window. On this box "claude"/"codex" are real installed CLIs (their
# --dangerously-skip-permissions forms are just interactive-shell aliases
# layered on top, which a non-interactive #!/bin/sh exec skips anyway) — so
# leaving WORKBENCH_AGENT unset would actually launch a live nested coding
# agent in a detached pane as a side effect of this test. Force it to
# "opencode", which is a valid enum value for mux-agent but is NOT installed
# here, so its `exec` just fails harmlessly inside the pane instead.
WORKBENCH_AGENT=opencode
export WORKBENCH_AGENT

# ============================================================================
# Scenario 1 — feature name, zero repos
# ============================================================================
feature1="onlyfeat"

out1="$WB_TEST_TMPDIR/ws-new-scenario1.out"
timeout 10 "$WSNEW" "$feature1" >"$out1" 2>&1 || true
# (ignore this command's own exit status — see helpers.sh gotcha #2)

wb_assert "scenario1: session '$feature1' exists" tmux has-session -t "=$feature1"

win_count1=$(tmux list-windows -t "$feature1" | wc -l | tr -d ' ')
wb_assert "scenario1: exactly one window" test "$win_count1" -eq 1

win_name1=$(tmux list-windows -t "$feature1" -F '#{window_name}')
wb_assert "scenario1: the one window is named 'agent'" test "$win_name1" = agent

wsdir1="$WORKBENCH_WORKSPACE_ROOT/$feature1"
wb_assert "scenario1: workspace dir exists" test -d "$wsdir1"

wsdir1_entries=$(ls -A "$wsdir1" | wc -l | tr -d ' ')
wb_assert "scenario1: workspace dir is empty (no repos given)" test "$wsdir1_entries" -eq 0

# ============================================================================
# Scenario 2 — feature name + two repos (plain name, repo:branch)
# ============================================================================
feature2="wsfeat"
repo_a="alpha"          # plain name -> branch defaults to the feature name
repo_b="beta"
repo_b_branch="beta-custom"

mk_fake_repo() {
  d="$WORKBENCH_CODE_ROOT/$1"
  remote="$WB_TEST_TMPDIR/remotes/$1.git"
  mkdir -p "$d"
  git -C "$d" init -q -b main
  git -C "$d" config user.email test@example.com
  git -C "$d" config user.name "Test User"
  echo seed > "$d/seed.txt"
  git -C "$d" add seed.txt
  git -C "$d" commit -q -m seed
  mkdir -p "$(dirname "$remote")"
  git clone -q --bare "$d" "$remote"
  git -C "$d" remote add origin "$remote"
  git -C "$d" fetch -q origin
  git -C "$d" remote set-head origin -a >/dev/null
}
mk_fake_repo "$repo_a"
mk_fake_repo "$repo_b"

out2="$WB_TEST_TMPDIR/ws-new-scenario2.out"
timeout 10 "$WSNEW" "$feature2" "$repo_a" "$repo_b:$repo_b_branch" >"$out2" 2>&1 || true
# (ignore this command's own exit status — see helpers.sh gotcha #2)

wb_assert "scenario2: session '$feature2' exists" tmux has-session -t "=$feature2"

# --- fake CODE_ROOT main checkouts are untouched ---------------------------
main_a_branch=$(git -C "$WORKBENCH_CODE_ROOT/$repo_a" rev-parse --abbrev-ref HEAD)
wb_assert "scenario2: $repo_a main checkout still on 'main'" test "$main_a_branch" = main
main_b_branch=$(git -C "$WORKBENCH_CODE_ROOT/$repo_b" rev-parse --abbrev-ref HEAD)
wb_assert "scenario2: $repo_b main checkout still on 'main'" test "$main_b_branch" = main

main_a_worktrees=$(git -C "$WORKBENCH_CODE_ROOT/$repo_a" worktree list | wc -l | tr -d ' ')
wb_assert "scenario2: $repo_a main checkout gained a second worktree" test "$main_a_worktrees" -eq 2
main_b_worktrees=$(git -C "$WORKBENCH_CODE_ROOT/$repo_b" worktree list | wc -l | tr -d ' ')
wb_assert "scenario2: $repo_b main checkout gained a second worktree" test "$main_b_worktrees" -eq 2

# --- worktrees exist under WORKBENCH_WORKSPACE_ROOT/<feature>/<repo> -------
wsdir2="$WORKBENCH_WORKSPACE_ROOT/$feature2"
wb_assert "scenario2: $repo_a worktree exists" test -d "$wsdir2/$repo_a"
wb_assert "scenario2: $repo_b worktree exists" test -d "$wsdir2/$repo_b"

wt_a_branch=$(git -C "$wsdir2/$repo_a" rev-parse --abbrev-ref HEAD)
wb_assert "scenario2: $repo_a worktree is on branch '$feature2' (plain-name default)" \
  test "$wt_a_branch" = "$feature2"

wt_b_branch=$(git -C "$wsdir2/$repo_b" rev-parse --abbrev-ref HEAD)
wb_assert "scenario2: $repo_b worktree is on branch '$repo_b_branch' (repo:branch override)" \
  test "$wt_b_branch" = "$repo_b_branch"

# --- session has the agent window + one inspection window per repo --------
win_count2=$(tmux list-windows -t "$feature2" | wc -l | tr -d ' ')
wb_assert "scenario2: three windows total (agent + 2 repos)" test "$win_count2" -eq 3

wb_assert "scenario2: agent window present" \
  sh -c "tmux list-windows -t '$feature2' -F '#{window_name}' | grep -qxF agent"
wb_assert "scenario2: $repo_a inspection window present" \
  sh -c "tmux list-windows -t '$feature2' -F '#{window_name}' | grep -qxF '$repo_a'"
wb_assert "scenario2: $repo_b inspection window present" \
  sh -c "tmux list-windows -t '$feature2' -F '#{window_name}' | grep -qxF '$repo_b'"

panes_a=$(tmux list-panes -t "$feature2:$repo_a" | wc -l | tr -d ' ')
wb_assert "scenario2: $repo_a inspection window has 3 panes" test "$panes_a" -eq 3
panes_b=$(tmux list-panes -t "$feature2:$repo_b" | wc -l | tr -d ' ')
wb_assert "scenario2: $repo_b inspection window has 3 panes" test "$panes_b" -eq 3

wb_test_report
