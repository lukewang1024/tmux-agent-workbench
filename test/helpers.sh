# test/helpers.sh — shared test harness for tmux-agent-workbench's
# test/test-*.sh scripts. Sourced (NOT executed) — it only defines functions
# and a couple of counters, it does nothing on its own. Typical test script:
#
#   #!/bin/sh
#   . "$(cd "$(dirname "$0")" && pwd)/helpers.sh"
#   wb_test_setup "wb-test-ws-new-$$"
#   trap wb_test_teardown EXIT
#
#   ...drive ws-new / ws-add / mux-inspect etc., then...
#
#   wb_assert "session exists" tmux has-session -t myfeature
#   wb_assert "worktree exists" test -d "$WORKBENCH_WORKSPACE_ROOT/myfeature/somerepo"
#
#   wb_test_report
#
# Two gotchas are baked in here, both hit for real while building this
# harness. Do not "simplify" either one back out:
#
#  1. Non-recursive tmux wrapper. wb_test_setup captures the REAL tmux
#     binary's absolute path via `command -v tmux` BEFORE putting anything
#     else on PATH, then hardcodes that absolute path inside a wrapper
#     script:  printf '#!/bin/sh\nexec %s -L %s "$@"\n' "$real_tmux" "$socket"
#     If the wrapper instead execs the bare name "tmux", and its own temp bin
#     dir is already first on PATH by the time it runs (which it will be,
#     since wb_test_setup puts it there), the wrapper calls itself forever
#     and the test hangs. Always hardcode the real absolute path, never bare
#     "tmux", inside the wrapper.
#
#  2. ws-new's and ws-promote's final step execs `tmux attach` / `tmux
#     switch-client`. There is no real TTY client under a headless test run,
#     so that step legitimately prints "no current client" on stderr and
#     exits non-zero — that is expected/OK, not a test failure. Callers MUST
#     wrap that specific invocation in `timeout 10 ...` (so a genuine hang
#     doesn't wedge the whole suite) and MUST NOT assert on that step's own
#     exit code. Assert on the resulting state instead — `tmux list-windows`,
#     `git worktree list`, etc. For example:
#       timeout 10 ws-new myfeature somerepo >/tmp/out.$$ 2>&1
#       # (ignore that command's own exit status)
#       wb_assert "session myfeature exists" tmux has-session -t myfeature
#
#  3. The wrapper's `-L <socket>` does NOT stop tmux from reading the
#     invoking user's real `~/.tmux.conf` on that new server's first command —
#     `-L` isolates which server you talk to, not which config it loads. In
#     practice that means this test suite's servers really do run with
#     whatever plugins/hooks are in the real live config (tmux-agent-sidebar's
#     auto-create-on-new-window included), not some neutral blank tmux. This
#     was DISCOVERED, not designed: a "no sidebar plugin loaded" test still
#     grew a sidebar pane. Do not "fix" this by adding `-f /dev/null` — the
#     opposite is more valuable: it makes this suite a real integration check
#     against the user's actual config.
#
#     tmux-agent-sidebar's auto-create is intentionally allowed on EVERY
#     window in this codebase, inspection windows included — its full-height
#     split narrows an existing layout uniformly rather than disturbing it,
#     so there's nothing to protect windows from. Practical consequence: an
#     exact pane-count assertion (e.g. "inspection window has 3 panes") is
#     only ever true because this suite's assertions run well inside
#     @sidebar_auto_create_delay (0.5s) — before the sidebar plugin's own
#     deferred hook has fired, not because anything here prevents it from
#     firing. That's a real, if currently-quiet, source of flakiness under
#     enough load; if these ever start failing intermittently with one more
#     pane than expected, this is why — don't chase it as a regression.

# ---------------------------------------------------------------------------
# running counters — namespaced so a test-*.sh script's own locals never
# collide with the harness's bookkeeping
# ---------------------------------------------------------------------------
WB_TEST_FAILCOUNT=${WB_TEST_FAILCOUNT:-0}
WB_TEST_ASSERTCOUNT=${WB_TEST_ASSERTCOUNT:-0}
WB_TEST_SOCKET=${WB_TEST_SOCKET:-}
WB_TEST_TMPDIR=${WB_TEST_TMPDIR:-}
WB_TEST_BINDIR=${WB_TEST_BINDIR:-}

# wb_assert <message> <command...> — run <command...> as an actual argv
# (never a string to eval). Prints "PASS: <message>" or "FAIL: <message>"
# and bumps the running failure counter on FAIL. Examples:
#   wb_assert "session exists" tmux has-session -t "$feature"
#   wb_assert "worktree dir exists" test -d "$dest"
wb_assert() {
  msg=$1; shift
  WB_TEST_ASSERTCOUNT=$((WB_TEST_ASSERTCOUNT + 1))
  if "$@"; then
    printf 'PASS: %s\n' "$msg"
    return 0
  else
    printf 'FAIL: %s\n' "$msg"
    WB_TEST_FAILCOUNT=$((WB_TEST_FAILCOUNT + 1))
    return 1
  fi
}

# wb_test_setup <unique-socket-name>
#   - kills any stale tmux server already bound to that socket name (from a
#     previous crashed/interrupted run), ignoring failure
#   - creates a fresh temp dir with fake CODE_ROOT/WORKSPACE_ROOT subdirs
#   - writes the non-recursive tmux wrapper (gotcha #1 above) into a temp bin
#     dir, then prepends that dir to PATH for the rest of the calling script
#   - exports WORKBENCH_CODE_ROOT / WORKBENCH_WORKSPACE_ROOT pointing at the
#     fake dirs
#
# Leaves these set for the calling script (and wb_test_teardown) to use:
#   WB_TEST_SOCKET  the socket name passed in
#   WB_TEST_TMPDIR  the temp dir root (contains bin/, code/, workspace/)
#   WB_TEST_BINDIR  the temp bin dir now first on PATH (holds the wrapper)
wb_test_setup() {
  [ $# -eq 1 ] || { echo "wb_test_setup: usage: wb_test_setup <socket-name>" >&2; return 2; }
  WB_TEST_SOCKET=$1

  # Capture the REAL tmux's absolute path BEFORE anything is prepended to
  # PATH (gotcha #1). Once our temp bin dir is on PATH, `command -v tmux`
  # would resolve to the wrapper itself instead of the real binary.
  real_tmux=$(command -v tmux) || { echo "wb_test_setup: no tmux found on PATH" >&2; return 1; }

  # Best-effort cleanup of a stale server left over from an earlier run
  # under this same socket name — there may be none, that's fine.
  "$real_tmux" -L "$WB_TEST_SOCKET" kill-server >/dev/null 2>&1 || true

  WB_TEST_TMPDIR=$(mktemp -d "${TMPDIR:-/tmp}/wb-test.${WB_TEST_SOCKET}.XXXXXX") || {
    echo "wb_test_setup: mktemp -d failed" >&2
    return 1
  }
  WB_TEST_BINDIR="$WB_TEST_TMPDIR/bin"
  mkdir -p "$WB_TEST_BINDIR" "$WB_TEST_TMPDIR/code" "$WB_TEST_TMPDIR/workspace"

  # The non-recursive tmux wrapper (gotcha #1): hardcode the real absolute
  # path, never the bare name "tmux". Pins every call this test makes to our
  # own isolated socket, so it can never touch (or collide with) whatever
  # real tmux server the caller happens to be running inside.
  printf '#!/bin/sh\nexec %s -L %s "$@"\n' "$real_tmux" "$WB_TEST_SOCKET" > "$WB_TEST_BINDIR/tmux"
  chmod +x "$WB_TEST_BINDIR/tmux"

  # Exercise this checkout's public API, independent of a user's installation.
  wb_test_repo=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd -P)
  ln -s "$wb_test_repo/bin/tmux-agent-workbench-cli" "$WB_TEST_BINDIR/tmux-agent-workbench"

  PATH="$WB_TEST_BINDIR:$PATH"
  export PATH

  WORKBENCH_CODE_ROOT="$WB_TEST_TMPDIR/code"
  WORKBENCH_WORKSPACE_ROOT="$WB_TEST_TMPDIR/workspace"
  export WORKBENCH_CODE_ROOT WORKBENCH_WORKSPACE_ROOT
}

# wb_test_teardown — kill the isolated tmux server for WB_TEST_SOCKET and
# rm -rf the temp dir tree created by wb_test_setup. Safe to call even if
# setup was never run or only partially succeeded.
wb_test_teardown() {
  if [ -n "$WB_TEST_SOCKET" ]; then
    # Goes through the wrapper on PATH (already pinned to our socket), so no
    # need to pass -L again here.
    tmux kill-server >/dev/null 2>&1 || true
  fi
  if [ -n "$WB_TEST_TMPDIR" ] && [ -d "$WB_TEST_TMPDIR" ]; then
    rm -rf "$WB_TEST_TMPDIR"
  fi
}

# wb_test_report — call once, at the end of a test-*.sh script. Prints a
# summary line and exits the script 0 if every wb_assert call so far passed,
# 1 otherwise.
wb_test_report() {
  echo "----------------------------------------"
  if [ "$WB_TEST_FAILCOUNT" -eq 0 ]; then
    printf 'PASS: %d/%d assertions passed\n' "$WB_TEST_ASSERTCOUNT" "$WB_TEST_ASSERTCOUNT"
    exit 0
  else
    printf 'FAIL: %d/%d assertions failed\n' "$WB_TEST_FAILCOUNT" "$WB_TEST_ASSERTCOUNT"
    exit 1
  fi
}
