#!/usr/bin/env bash
# Contract test for tmux's session-name and target-grammar behaviour (issue #66).
#
# test/target-resolution-unit-tests.sh drives the same functions against a
# fabricated pane table, which cannot notice tmux changing what it emits or what
# it accepts. This one asks a real tmux, so it is the layer that catches an
# upstream change — the same split the repo already uses for Copilot
# (contract / end-to-end / hermetic).
#
# Two things are pinned:
#   1. resolve_tmux_pane_id() finds a pane whose session name contains ':', '.'
#      or '|', and the pane id it returns is accepted by every tmux command the
#      restore hook issues.
#   2. The saved "session:window.pane" label is NOT a usable target for those
#      names. If that assertion ever fails, tmux changed its grammar and the
#      workaround may be simplifiable — it does not mean the plugin is broken.
#
# Needs a tmux >= 3.7: earlier versions silently rewrite ':' and '.' in session
# names to '_', which makes the interesting cases unreachable. Skips cleanly
# when tmux is absent or too old, so it is safe to run anywhere.
#
# Runs on its own socket with an empty config, so a developer's live tmux server
# is never touched. Run with:  bash test/tmux-target-contract-test.sh
#                         (or: just test-tmux-contract)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

if ! command -v tmux >/dev/null 2>&1; then
	echo "SKIP: tmux is not installed"
	exit 0
fi

REAL_TMUX="$(command -v tmux)"
# A socket *path* under a fresh mktemp directory, not a `-L` label: a label is
# derived from a shared per-user directory, so a stale one could already exist
# and this test's kill-server would take down somebody else's server. The path
# is guaranteed not to exist yet, so the server here is provably this test's.
SOCKET_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tar-target-contract.XXXXXX")"
SOCKET="$SOCKET_DIR/socket"

# Every tmux call below — including the ones inside resolve_tmux_pane_id() —
# goes to a private server with no config loaded.
tmux() { "$REAL_TMUX" -S "$SOCKET" -f /dev/null "$@"; }
cleanup() {
	"$REAL_TMUX" -S "$SOCKET" kill-server >/dev/null 2>&1 || true
	rm -rf "$SOCKET_DIR"
}
trap cleanup EXIT INT TERM

# shellcheck source=../scripts/lib-detect.sh
source "$REPO_DIR/scripts/lib-detect.sh"

PASS=0
FAIL=0
assert_eq() {
	local desc="$1" expected="$2" actual="$3"
	if [ "$expected" = "$actual" ]; then
		PASS=$((PASS + 1))
		printf '  [pass] %s\n' "$desc"
	else
		FAIL=$((FAIL + 1))
		printf '  [FAIL] %s\n        expected: [%s]\n        actual:   [%s]\n' "$desc" "$expected" "$actual"
	fi
}

echo "tmux: $("$REAL_TMUX" -V)"

# --- version gate, asked rather than parsed ---
#
# A version string is the wrong question: what matters is whether this tmux
# keeps the characters, and `-P -F` reports the name it actually created.
PROBE='v1.2:x'
probe_actual=$(tmux new-session -d -P -F '#{session_name}' -s "$PROBE" -c /tmp 2>/dev/null)
if [ "$probe_actual" != "$PROBE" ]; then
	echo "SKIP: tmux rewrote '$PROBE' to '${probe_actual:-<rejected>}' — needs tmux >= 3.7"
	exit 0
fi

echo "== session names round-trip through tmux =="
# Each of these breaks a different assumption: '.' and ':' are the target
# grammar's separators, '|' is this plugin's -F delimiter, and the URL is the
# realistic case from issue #66 that also triggers tmux's prefix matching.
NAMES=('v1.2' 'a:b' 'has|pipe' 'https://github.com/timvw/x' 'plain')
for name in "${NAMES[@]}"; do
	created=$(tmux new-session -d -P -F '#{session_name}' -s "$name" -c /tmp 2>/dev/null)
	assert_eq "tmux keeps '$name' verbatim" "$name" "$created"
done

echo "== resolve_tmux_pane_id finds each pane in real list-panes output =="
declare -a RESOLVED=()
for name in "${NAMES[@]}"; do
	pane_id=$(resolve_tmux_pane_id "$name" 0 0)
	case "$pane_id" in
	%[0-9]*)
		PASS=$((PASS + 1))
		printf "  [pass] '%s' resolved to a pane id (%s)\n" "$name" "$pane_id"
		;;
	*)
		FAIL=$((FAIL + 1))
		printf "  [FAIL] '%s' did not resolve to a pane id (got [%s])\n" "$name" "$pane_id"
		;;
	esac
	RESOLVED+=("$pane_id")
done

echo "== the resolved pane id round-trips back to the same session =="
for i in "${!NAMES[@]}"; do
	name="${NAMES[$i]}"
	pane_id="${RESOLVED[$i]}"
	[ -n "$pane_id" ] || continue
	assert_eq "'$name' -> $pane_id -> same session name" "$name" \
		"$(tmux display-message -t "$pane_id" -p '#{session_name}' 2>/dev/null)"
done

echo "== every tmux command the restore hook issues accepts a pane id =="
# If any of these rejected a pane id, restore would have to fall back to a name
# target, which is the thing that cannot be made safe.
probe_id=$(resolve_tmux_pane_id 'https://github.com/timvw/x' 0 0)
for cmd in \
	"has-session" \
	"list-clients" \
	"clear-history"; do
	if tmux "$cmd" -t "$probe_id" >/dev/null 2>&1; then
		PASS=$((PASS + 1))
		printf '  [pass] tmux %s accepts a pane id target\n' "$cmd"
	else
		FAIL=$((FAIL + 1))
		printf '  [FAIL] tmux %s rejected pane id %s\n' "$cmd" "$probe_id"
	fi
done
assert_eq "display-message reads pane_current_command via pane id" "yes" \
	"$([ -n "$(tmux display-message -t "$probe_id" -p '#{pane_current_command}' 2>/dev/null)" ] && echo yes || echo no)"
assert_eq "display-message reads pane_pid via pane id" "yes" \
	"$([ -n "$(tmux display-message -t "$probe_id" -p '#{pane_pid}' 2>/dev/null)" ] && echo yes || echo no)"
if tmux send-keys -t "$probe_id" "" 2>/dev/null; then
	PASS=$((PASS + 1))
	printf '  [pass] tmux send-keys accepts a pane id target\n'
else
	FAIL=$((FAIL + 1))
	printf '  [FAIL] tmux send-keys rejected pane id %s\n' "$probe_id"
fi

echo "== the saved label is still not a usable target (why the fix exists) =="
# These pin tmux's current behaviour, not the plugin's. A failure here means
# tmux changed its target grammar -- worth knowing, and grounds for revisiting
# resolve_tmux_pane_id(), not a defect in this repo.
if tmux has-session -t 'v1.2' >/dev/null 2>&1; then
	FAIL=$((FAIL + 1))
	printf "  [FAIL] tmux now accepts 'v1.2' as a session target — grammar changed\n"
else
	PASS=$((PASS + 1))
	printf "  [pass] tmux still rejects 'v1.2' as a session target ('.' is reserved)\n"
fi
if tmux list-panes -t 'https://github.com/timvw/x:0.0' >/dev/null 2>&1; then
	FAIL=$((FAIL + 1))
	printf '  [FAIL] tmux now accepts a URL-named pane label as a target — grammar changed\n'
else
	PASS=$((PASS + 1))
	printf '  [pass] tmux still rejects a URL-named pane label as a target\n'
fi
# Prefix matching is the dangerous half: this does not fail, it silently picks
# a different session, which is how a pane gets restored into the wrong place.
prefix_hit=$(tmux display-message -t 'https' -p '#{session_name}' 2>/dev/null)
if [ "$prefix_hit" = "https://github.com/timvw/x" ]; then
	PASS=$((PASS + 1))
	printf "  [pass] tmux still prefix-matches 'https' to '%s' (silent wrong-session hazard)\n" "$prefix_hit"
else
	FAIL=$((FAIL + 1))
	printf "  [FAIL] tmux prefix matching changed: 'https' gave [%s]\n" "$prefix_hit"
fi

# The probe session from the version gate is still around; drop everything.
cleanup

echo
echo "tmux target contract tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
