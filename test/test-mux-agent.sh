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
EOF
chmod +x "$WB_TEST_BINDIR/traex"

MUX_AGENT_TEST_OUTPUT=$out WORKBENCH_AGENT=trae "$mux_agent"
wb_assert "mux-agent dispatches the trae family to traex" test -s "$out"
wb_assert "mux-agent exports source identity for later handoffs" test "$(cat "$out")" = 'trae|trae-default'

wb_test_report
