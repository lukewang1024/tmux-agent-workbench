#!/bin/sh
set -u

dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

handoff="$dir/../bin/mux-handoff"
wb_test_setup "wb-test-mux-handoff-$$"
trap wb_test_teardown EXIT

export XDG_CONFIG_HOME="$WB_TEST_TMPDIR/config"
profile_dir="$XDG_CONFIG_HOME/tmux-agent-workbench/profiles"
mkdir -p "$profile_dir"

received="$WB_TEST_TMPDIR/received-prompt"
cat > "$WB_TEST_BINDIR/fake-handoff-agent" <<'EOF'
#!/bin/sh
set -eu
cp "$1" "$HANDOFF_TEST_OUTPUT"
printf '%s' "${HANDOFF_FIXED_ENV:-}" > "$HANDOFF_TEST_OUTPUT.env"
exec sleep 30
EOF
chmod +x "$WB_TEST_BINDIR/fake-handoff-agent"

cat > "$profile_dir/test-success.conf" <<EOF
adapter=command
description=Test successor
command=$WB_TEST_BINDIR/fake-handoff-agent
env.HANDOFF_TEST_OUTPUT=$received
env.HANDOFF_FIXED_ENV=from-profile
EOF

cat > "$profile_dir/test-failure.conf" <<'EOF'
adapter=command
description=Missing test successor
command=definitely-not-a-real-handoff-command
EOF

new_source()
{
  name=$1
  tmux new-session -d -s "$name" -n agent -c "$WB_TEST_TMPDIR" 'exec sleep 30'
  tmux list-panes -t "$name:agent" -F '#{pane_id}'
}

tmux_context()
{
  pane=$1
  sockpath=$(tmux display-message -p -t "$pane" '#{socket_path}')
  pid=$(tmux display-message -p -t "$pane" '#{pid}')
  sessid=$(tmux display-message -p -t "$pane" '#{session_id}')
  printf '%s,%s,%s\n' "$sockpath" "$pid" "${sessid#\$}"
}

run_handoff()
{
  pane=$1
  shift
  TMUX=$(tmux_context "$pane") TMUX_PANE=$pane "$handoff" "$@"
}

pane_is_absent()
{
  wanted=$1
  ! tmux list-panes -a -F '#{pane_id}' | grep -qxF "$wanted"
}

source1=$(new_source handoff-success)
if printf 'Continue frobnicator work.\n' | run_handoff "$source1" \
    --target test-success --startup-timeout 3 --no-close >"$WB_TEST_TMPDIR/success.out" 2>&1; then
  success_rc=0
else
  success_rc=$?
fi
wb_assert "successful handoff exits 0" test "$success_rc" -eq 0
wb_assert "source remains with --no-close" tmux display-message -p -t "$source1" '#{pane_id}'
target1=$(tmux list-panes -t handoff-success:agent -F '#{pane_id}' | grep -vxF "$source1")
wb_assert "target pane exists" tmux display-message -p -t "$target1" '#{pane_id}'
wb_assert "target becomes active while source window is active" test "$(tmux display-message -p -t "$target1" '#{pane_active}')" = 1
wb_assert "free-text summary reached successor" grep -q 'Continue frobnicator work.' "$received"
physical_tmp=$(cd "$WB_TEST_TMPDIR" && pwd -P)
wb_assert "mechanical cwd reached successor" grep -q "cwd: $physical_tmp" "$received"
wb_assert "target profile reached successor" grep -q 'target_profile: test-success' "$received"
wb_assert "fixed profile environment reached launcher" test "$(cat "$received.env")" = from-profile
wb_assert "source lock clears for --no-close" test -z "$(tmux show-option -pqv -t "$source1" @handoff_in_progress 2>/dev/null || true)"

# Built-in adapters construct each CLI's real interactive initial-prompt argv.
# Fake binaries prove the flags without starting paid/network agent sessions.
adapter_log_dir="$WB_TEST_TMPDIR/adapter-logs"
mkdir -p "$adapter_log_dir"
cat > "$WB_TEST_BINDIR/fake-builtin-agent" <<'EOF'
#!/bin/sh
set -eu
out="$ADAPTER_LOG_DIR/$(basename "$0")"
printf '<%s>\n' "$@" > "$out"
exec sleep 30
EOF
chmod +x "$WB_TEST_BINDIR/fake-builtin-agent"
for executable in codex claude traex opencode; do
  ln -s "$WB_TEST_BINDIR/fake-builtin-agent" "$WB_TEST_BINDIR/$executable"
done

cat > "$profile_dir/test-codex.conf" <<EOF
adapter=codex
description=Codex argv test
model=codex-test
effort=high
permissions=bypass
env.ADAPTER_LOG_DIR=$adapter_log_dir
EOF
cat > "$profile_dir/test-claude.conf" <<EOF
adapter=claude
description=Claude argv test
model=opus
effort=high
permissions=bypass
env.ADAPTER_LOG_DIR=$adapter_log_dir
EOF
cat > "$profile_dir/test-trae.conf" <<EOF
adapter=trae
description=Trae argv test
model=trae-test
permissions=bypass
env.ADAPTER_LOG_DIR=$adapter_log_dir
EOF
cat > "$profile_dir/test-opencode.conf" <<EOF
adapter=opencode
description=opencode argv test
model=provider/test
permissions=bypass
env.ADAPTER_LOG_DIR=$adapter_log_dir
EOF

for adapter in codex claude trae opencode; do
  adapter_source=$(new_source "handoff-adapter-$adapter")
  printf 'Adapter %s prompt.\n' "$adapter" | run_handoff "$adapter_source" \
    --target "test-$adapter" --startup-timeout 3 --no-close \
    >"$WB_TEST_TMPDIR/adapter-$adapter.out" 2>&1
done
wb_assert "codex adapter enables bypass" grep -qxF '<--yolo>' "$adapter_log_dir/codex"
wb_assert "codex adapter passes model" grep -qxF '<codex-test>' "$adapter_log_dir/codex"
wb_assert "codex adapter passes reasoning effort" grep -qxF '<model_reasoning_effort="high">' "$adapter_log_dir/codex"
wb_assert "claude adapter enables bypass" grep -qxF '<--dangerously-skip-permissions>' "$adapter_log_dir/claude"
wb_assert "claude adapter passes model" grep -qxF '<opus>' "$adapter_log_dir/claude"
wb_assert "claude adapter passes effort" grep -qxF '<high>' "$adapter_log_dir/claude"
wb_assert "trae adapter enables bypass" grep -qxF '<--dangerously-bypass-approvals-and-sandbox>' "$adapter_log_dir/traex"
wb_assert "trae adapter passes model" grep -qxF '<trae-test>' "$adapter_log_dir/traex"
wb_assert "opencode adapter enables auto approval" grep -qxF '<--auto>' "$adapter_log_dir/opencode"
wb_assert "opencode adapter uses prompt option" grep -qxF '<--prompt>' "$adapter_log_dir/opencode"
wb_assert "built-in adapter prompt contains handoff instruction" grep -q 'taking over an in-progress coding task' "$adapter_log_dir/codex"

source2=$(new_source handoff-failure)
before=$(tmux list-panes -t handoff-failure:agent | wc -l | tr -d ' ')
if printf 'This should fail.\n' | run_handoff "$source2" \
    --target test-failure --startup-timeout 2 >"$WB_TEST_TMPDIR/failure.out" 2>&1; then
  failure_rc=0
else
  failure_rc=$?
fi
after=$(tmux list-panes -t handoff-failure:agent | wc -l | tr -d ' ')
wb_assert "failed target returns nonzero" test "$failure_rc" -ne 0
wb_assert "source survives failed target" tmux display-message -p -t "$source2" '#{pane_id}'
wb_assert "failed target pane is cleaned" test "$after" -eq "$before"
wb_assert "failed handoff releases source lock" test -z "$(tmux show-option -pqv -t "$source2" @handoff_in_progress 2>/dev/null || true)"

source3=$(new_source handoff-cancel)
sed "s|^env.HANDOFF_TEST_OUTPUT=.*|env.HANDOFF_TEST_OUTPUT=$WB_TEST_TMPDIR/cancel-prompt|" \
  "$profile_dir/test-success.conf" > "$profile_dir/test-cancel.conf"
printf 'Cancel case.\n' | run_handoff "$source3" --target test-cancel --timeout 2 --startup-timeout 3 \
  >"$WB_TEST_TMPDIR/cancel-start.out" 2>&1
target3=$(tmux show-option -pqv -t "$source3" @handoff_target_pane)
TMUX=$(tmux_context "$target3") TMUX_PANE=$target3 "$handoff" cancel >"$WB_TEST_TMPDIR/cancel.out"
sleep 3
wb_assert "cancel from target preserves source" tmux display-message -p -t "$source3" '#{pane_id}'
wb_assert "cancel preserves target" tmux display-message -p -t "$target3" '#{pane_id}'
wb_assert "cancel clears source lock" test -z "$(tmux show-option -pqv -t "$source3" @handoff_in_progress 2>/dev/null || true)"

source4=$(new_source handoff-watchdog)
sed "s|^env.HANDOFF_TEST_OUTPUT=.*|env.HANDOFF_TEST_OUTPUT=$WB_TEST_TMPDIR/watchdog-prompt|" \
  "$profile_dir/test-success.conf" > "$profile_dir/test-watchdog.conf"
printf 'Watchdog case.\n' | run_handoff "$source4" --target test-watchdog --timeout 1 --startup-timeout 3 \
  >"$WB_TEST_TMPDIR/watchdog.out" 2>&1
target4=$(tmux show-option -pqv -t "$source4" @handoff_target_pane)
sleep 4
wb_assert "watchdog closes source" pane_is_absent "$source4"
wb_assert "watchdog preserves target" tmux display-message -p -t "$target4" '#{pane_id}'
wb_assert "watchdog clears target pairing" test -z "$(tmux show-option -pqv -t "$target4" @handoff_source_pane 2>/dev/null || true)"

source5=$(new_source handoff-focus)
tmux new-window -d -t handoff-focus: -n elsewhere 'exec sleep 30'
tmux select-window -t handoff-focus:elsewhere
sed "s|^env.HANDOFF_TEST_OUTPUT=.*|env.HANDOFF_TEST_OUTPUT=$WB_TEST_TMPDIR/focus-prompt|" \
  "$profile_dir/test-success.conf" > "$profile_dir/test-focus.conf"
printf 'Focus case.\n' | run_handoff "$source5" --target test-focus --no-close --startup-timeout 3 \
  >"$WB_TEST_TMPDIR/focus.out" 2>&1
active_window=$(tmux display-message -p -t handoff-focus '#{window_name}')
wb_assert "handoff does not pull user from another window" test "$active_window" = elsewhere

wb_test_report
