#!/bin/sh
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
. "$dir/helpers.sh"

wb_test_setup "wb-test-session-pick-$$"
trap wb_test_teardown EXIT

repo=$(cd "$dir/.." && pwd -P)
fake_bin=$WB_TEST_TMPDIR/bin
calls=$WB_TEST_TMPDIR/calls
mkdir -p "$fake_bin"

cat > "$fake_bin/tmuxinator" <<'EOF'
#!/bin/sh
case $1 in
  list) printf '%s\n' 'tmuxinator projects:' alpha beta ;;
  start) printf 'start %s\n' "$2" >> "$WORKBENCH_TEST_CALLS" ;;
esac
EOF
cat > "$fake_bin/fzf-tmux" <<'EOF'
#!/bin/sh
sed -n '2p'
EOF
chmod 755 "$fake_bin/tmuxinator" "$fake_bin/fzf-tmux"

output=$(PATH=$fake_bin:$PATH "$repo/bin/workbench-session-pick" --list)
wb_assert "project list removes tmuxinator heading" test "$output" = "alpha
beta"

TMUX=test WORKBENCH_TEST_CALLS=$calls PATH=$fake_bin:$PATH \
  "$repo/bin/tmux-agent-workbench-cli" pick project
wb_assert "picker starts selected tmuxinator project" grep -qx 'start beta' "$calls"

wb_test_report
