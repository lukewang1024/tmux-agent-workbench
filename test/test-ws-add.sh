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
mkdir -p "$repodir/.workbench"
cat > "$repodir/.workbench/worktree-init" <<'EOF'
#!/bin/sh
set -eu
[ "$WORKBENCH_INIT_PROTOCOL" = 1 ]
[ "$WORKBENCH_MAIN_CHECKOUT" != "$WORKBENCH_WORKTREE" ]
[ "$WORKBENCH_REPO" = somerepo ]
[ "$WORKBENCH_BRANCH" = in-progress-ws ]
[ "$PWD" = "$WORKBENCH_WORKTREE" ]
printf '%s\n' "$WORKBENCH_FEATURE|$WORKBENCH_WORKSPACE" >> "$WORKBENCH_WORKTREE/.initializer-runs"
EOF
chmod +x "$repodir/.workbench/worktree-init"
git -C "$repodir" add file.txt
git -C "$repodir" add .workbench/worktree-init
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
wb_assert "repository initializer ran" test -f "$dest/.initializer-runs"
wb_assert "repository initializer received workspace context" \
  grep -qxF "$feature|$WORKBENCH_WORKSPACE_ROOT/$feature" "$dest/.initializer-runs"
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
initruns=$(wc -l < "$dest/.initializer-runs" | tr -d ' ')
wb_assert "repository initializer reruns idempotently" [ "$initruns" -eq 2 ]

# ---------------------------------------------------------------------------
# 6. A failing initializer stops before mux-inspect but keeps the worktree so a
#    corrected initializer can repair it on the next ws-add invocation.
# ---------------------------------------------------------------------------
failrepo="failrepo"
failmain="$WORKBENCH_CODE_ROOT/$failrepo"
mkdir -p "$failmain/.workbench"
git -C "$failmain" init -q
git -C "$failmain" config user.email "test@example.com"
git -C "$failmain" config user.name "Test"
echo "failure fixture" > "$failmain/file.txt"
cat > "$failmain/.workbench/worktree-init" <<'EOF'
#!/bin/sh
exit 23
EOF
chmod +x "$failmain/.workbench/worktree-init"
git -C "$failmain" add file.txt .workbench/worktree-init
git -C "$failmain" commit -q -m "initial"

faildest="$WORKBENCH_WORKSPACE_ROOT/$feature/$failrepo"
failout=$("$WS_ADD" "$failrepo" 2>&1); failrc=$?
printf '%s\n' "$failout"
wb_assert "initializer failure preserves its exit code" [ "$failrc" -eq 23 ]
wb_assert "initializer failure keeps the created worktree" test -d "$faildest"
wb_assert "initializer failure stops before inspection window creation" \
  sh -c "! tmux list-windows -t '$feature' -F '#W' | grep -qxF '$failrepo'"

# ---------------------------------------------------------------------------
# 7. Unsafe initializer shapes are rejected without executing them. The
#    worktree is still preserved, matching the ordinary failure path.
# ---------------------------------------------------------------------------
for kind in symlink nonexec; do
  unsaferepo="${kind}repo"
  unsafemain="$WORKBENCH_CODE_ROOT/$unsaferepo"
  mkdir -p "$unsafemain/.workbench"
  git -C "$unsafemain" init -q
  git -C "$unsafemain" config user.email "test@example.com"
  git -C "$unsafemain" config user.name "Test"
  echo "unsafe fixture" > "$unsafemain/file.txt"
  if [ "$kind" = symlink ]; then
    cat > "$unsafemain/initializer-target" <<'EOF'
#!/bin/sh
touch "$WORKBENCH_WORKTREE/.initializer-should-not-run"
EOF
    chmod +x "$unsafemain/initializer-target"
    ln -s ../initializer-target "$unsafemain/.workbench/worktree-init"
    git -C "$unsafemain" add file.txt initializer-target .workbench/worktree-init
  else
    cat > "$unsafemain/.workbench/worktree-init" <<'EOF'
#!/bin/sh
touch "$WORKBENCH_WORKTREE/.initializer-should-not-run"
EOF
    chmod -x "$unsafemain/.workbench/worktree-init"
    git -C "$unsafemain" add file.txt .workbench/worktree-init
  fi
  git -C "$unsafemain" commit -q -m "initial"

  unsafeout=$("$WS_ADD" "$unsaferepo" 2>&1); unsaferc=$?
  printf '%s\n' "$unsafeout"
  unsafedest="$WORKBENCH_WORKSPACE_ROOT/$feature/$unsaferepo"
  wb_assert "$kind initializer is rejected" [ "$unsaferc" -ne 0 ]
  wb_assert "$kind initializer is not executed" test ! -e "$unsafedest/.initializer-should-not-run"
  wb_assert "$kind initializer leaves no inspection window" \
    sh -c "! tmux list-windows -t '$feature' -F '#W' | grep -qxF '$unsaferepo'"
done

wb_test_report
