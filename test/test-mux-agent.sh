#!/bin/sh
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

wb_test_setup "wb-test-mux-agent-$$"
trap wb_test_teardown EXIT

mux_agent="$dir/../bin/mux-agent"
out="$WB_TEST_TMPDIR/agent-env"
cat > "$WB_TEST_BINDIR/traex" <<'EOF'
#!/bin/sh
set -eu
printf '%s|%s\n' "$WORKBENCH_AGENT" "$WORKBENCH_PROFILE" > "$MUX_AGENT_TEST_OUTPUT"
if [ -n "${TMUX_PANE:-}" ]; then
  exec sleep 30
fi
EOF
chmod +x "$WB_TEST_BINDIR/traex"

MUX_AGENT_TEST_OUTPUT=$out WORKBENCH_AGENT=trae "$mux_agent"
wb_assert "mux-agent dispatches the trae family to traex" test -s "$out"
wb_assert "mux-agent exports source identity for later handoffs" test "$(cat "$out")" = 'trae|trae-default'

tmux new-session -d -s mux-agent-identity -n agent \
  "MUX_AGENT_TEST_OUTPUT='$out.pane' WORKBENCH_AGENT=trae WORKBENCH_PROFILE=trae-custom '$mux_agent'"
agent_pane=$(tmux list-panes -t mux-agent-identity:agent -F '#{pane_id}')
attempts=0
while [ ! -s "$out.pane" ] && [ "$attempts" -lt 30 ]; do
  sleep 0.1
  attempts=$((attempts + 1))
done
wb_assert "mux-agent stamps agent identity on its pane" test \
  "$(tmux show-option -pqv -t "$agent_pane" @workbench_agent)" = trae
wb_assert "mux-agent stamps profile identity on its pane" test \
  "$(tmux show-option -pqv -t "$agent_pane" @workbench_profile)" = trae-custom

wb_test_report
