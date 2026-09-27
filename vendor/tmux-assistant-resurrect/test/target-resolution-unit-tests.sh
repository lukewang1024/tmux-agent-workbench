#!/usr/bin/env bash
# Hermetic unit tests for saved-pane target resolution (issue #66).
#
# These need no tmux server: split_pane_target() is pure string work and
# match_pane_id() reads a `tmux list-panes -F` table on stdin, so the table is
# fabricated here. That is the point — the interesting session names contain
# ':' and '.', which tmux 3.4-3.6 silently rewrite to '_'. A test that asked a
# real tmux 3.4 (the Docker suite's version) for a session named "v1.2" would
# get "v1_2" and pass vacuously. Driving the functions directly covers the
# behaviour on every tmux version, including the ones CI cannot install.
#
# Run locally with:  bash test/target-resolution-unit-tests.sh  (or: just test-targets)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

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

# Call split_pane_target and render the outcome as "session|window|index", or
# "<rejected>" when it returns non-zero. The variables are cleared first so a
# rejected target cannot report the previous call's values.
split() {
	PANE_TARGET_SESSION="" PANE_TARGET_WINDOW="" PANE_TARGET_INDEX=""
	if split_pane_target "$1"; then
		printf '%s|%s|%s' "$PANE_TARGET_SESSION" "$PANE_TARGET_WINDOW" "$PANE_TARGET_INDEX"
	else
		printf '<rejected>'
	fi
}

echo "== split_pane_target: ordinary targets =="
assert_eq "plain name" "work|0|0" "$(split 'work:0.0')"
assert_eq "multi-digit window/pane" "work|12|3" "$(split 'work:12.3')"
assert_eq "name with spaces" "my work|1|2" "$(split 'my work:1.2')"

echo "== split_pane_target: names holding the grammar's own separators =="
# Left-splitting these is what issue #66 reports: "${t%%:*}" makes "v1.2:0.0"
# into "v1" and "https://h/x:0.0" into "https". Splitting from the right is
# exact, because window and pane are always indices.
assert_eq "dot in name" "v1.2|0|0" "$(split 'v1.2:0.0')"
assert_eq "colon in name" "a:b|0|0" "$(split 'a:b:0.0')"
assert_eq "colon and dot" "a:b.c|1|2" "$(split 'a:b.c:1.2')"
assert_eq "url-like name" "https://github.com/x|0|0" "$(split 'https://github.com/x:0.0')"
assert_eq "name is itself a target" "x:0.0|0|0" "$(split 'x:0.0:0.0')"
assert_eq "pipe in name" "has|pipe|0|0" "$(split 'has|pipe:0.0')"
assert_eq "backslash in name" "back\\slash|0|0" "$(split 'back\slash:0.0')"
assert_eq "trailing dot in name" "dot.|0|0" "$(split 'dot.:0.0')"

echo "== split_pane_target: malformed targets are rejected =="
assert_eq "no separators" "<rejected>" "$(split 'work')"
assert_eq "colon but no dot" "<rejected>" "$(split 'work:0')"
assert_eq "dot but no colon" "<rejected>" "$(split 'work.0')"
assert_eq "dot before colon only" "<rejected>" "$(split 'v1.2:0')"
assert_eq "empty target" "<rejected>" "$(split '')"

# A pane table shaped exactly like resolve_tmux_pane_id() feeds in:
#   #{pane_id}|#{window_index}|#{pane_index}|#{session_name}
# Session name last, since it is the only field that may contain the delimiter.
PANES='%0|0|0|plain
%1|0|0|v1.2
%2|0|0|a:b
%3|0|0|has|pipe
%4|1|2|work
%5|0|0|work
%6|0|0|back\slash
%7|0|0|plainer'

match() { printf '%s\n' "$PANES" | match_pane_id "$1" "$2" "$3"; }

echo "== match_pane_id: exact field matching =="
assert_eq "plain session" "%0" "$(match 'plain' 0 0)"
assert_eq "dot in name" "%1" "$(match 'v1.2' 0 0)"
assert_eq "colon in name" "%2" "$(match 'a:b' 0 0)"
assert_eq "pipe in name (delimiter in the value)" "%3" "$(match 'has|pipe' 0 0)"
assert_eq "backslash in name" "%6" "$(match 'back\slash' 0 0)"
assert_eq "window and pane index both honoured" "%4" "$(match 'work' 1 2)"
assert_eq "same session, different window" "%5" "$(match 'work' 0 0)"

echo "== match_pane_id: no partial or prefix matches =="
# tmux itself prefix-matches session names, which is how a saved "https" target
# silently resolves against a different session. Matching literally must not.
assert_eq "prefix of a real name" "" "$(match 'plai' 0 0)"
assert_eq "real name is a prefix of another" "%0" "$(match 'plain' 0 0)"
assert_eq "longer name not matched by prefix" "%7" "$(match 'plainer' 0 0)"
assert_eq "left-split remnant of a dotted name" "" "$(match 'v1' 0 0)"
assert_eq "left-split remnant of a colon name" "" "$(match 'a' 0 0)"
assert_eq "left-split remnant of a piped name" "" "$(match 'has' 0 0)"
assert_eq "unknown session" "" "$(match 'nope' 0 0)"
assert_eq "known session, wrong window" "" "$(match 'plain' 9 0)"
assert_eq "known session, wrong pane" "" "$(match 'plain' 0 9)"
assert_eq "empty session name" "" "$(match '' 0 0)"

echo "== match_pane_id: malformed input is skipped, not fatal =="
assert_eq "empty table" "" "$(printf '' | match_pane_id 'plain' 0 0)"
assert_eq "row with too few fields" "" "$(printf '%%0|0|plain\n' | match_pane_id 'plain' 0 0)"
assert_eq "blank line ignored, good row still found" "%0" \
	"$(printf '\n%%0|0|0|plain\n' | match_pane_id 'plain' 0 0)"
assert_eq "malformed row before a good one" "%0" \
	"$(printf 'garbage\n%%0|0|0|plain\n' | match_pane_id 'plain' 0 0)"

echo "== match_pane_id: one id per match =="
# Two rows cannot legitimately share session/window/pane, but if the table were
# ever ambiguous the caller must still receive a single usable target rather
# than a multi-line string that would be pasted into a tmux -t argument.
assert_eq "duplicate rows yield the first id only" "%0" \
	"$(printf '%%0|0|0|plain\n%%9|0|0|plain\n' | match_pane_id 'plain' 0 0)"

echo "== round trip: split_pane_target output feeds match_pane_id =="
# This is the legacy-sidecar path in restore-assistant-sessions.sh: a JSON entry
# written before session_name/window_index/pane_index existed carries only the
# composed "pane" string, so it has to be taken apart before it can be matched.
round_trip() {
	PANE_TARGET_SESSION="" PANE_TARGET_WINDOW="" PANE_TARGET_INDEX=""
	split_pane_target "$1" || return 1
	printf '%s\n' "$PANES" | match_pane_id "$PANE_TARGET_SESSION" "$PANE_TARGET_WINDOW" "$PANE_TARGET_INDEX"
}

assert_eq "plain:0.0 -> pane id" "%0" "$(round_trip 'plain:0.0')"
assert_eq "v1.2:0.0 -> pane id" "%1" "$(round_trip 'v1.2:0.0')"
assert_eq "a:b:0.0 -> pane id" "%2" "$(round_trip 'a:b:0.0')"
assert_eq "has|pipe:0.0 -> pane id" "%3" "$(round_trip 'has|pipe:0.0')"
assert_eq "work:1.2 -> pane id" "%4" "$(round_trip 'work:1.2')"
assert_eq "back\\slash:0.0 -> pane id" "%6" "$(round_trip 'back\slash:0.0')"

echo "== save side: the two pane records join on pane id, free-form field last =="
# Static guards, in the style of the issue #48 heredoc suite, because the awk
# that consumes these records is embedded in the save hook and cannot be driven
# in isolation. Two properties have to survive future edits:
#
#   1. The join key is #{pane_id}, not #{pane_pid}. Both are delimiter-free, so
#      swapping them looks harmless and no behavioural test would notice — but
#      the kernel may hand a dead pane's pid to a new pane between the two
#      list-panes calls, pairing one pane's metadata with another's cwd. tmux
#      never reuses a pane id within a server.
#   2. Each record ends with its one field that may contain the '|' delimiter
#      (the session name, the pane path). Everything ahead of it is peeled off
#      by position; a field appended after it would be swallowed by a session
#      named "a|b".
SAVE_SH="$REPO_DIR/scripts/save-assistant-sessions.sh"
pane_formats=$(grep -o 'list-panes -a -F "[^"]*"' "$SAVE_SH" | sed 's/.*-F "//; s/"$//')
p_format=$(printf '%s\n' "$pane_formats" | grep '^P|' || true)
c_format=$(printf '%s\n' "$pane_formats" | grep '^C|' || true)

starts_with() {
	case "$2" in "$1"*) echo yes ;; *) echo no ;; esac
}
contains() {
	case "$2" in *"$1"*) echo yes ;; *) echo no ;; esac
}

assert_eq "P record: tag, then pane id as the join key" "yes" "$(starts_with 'P|#{pane_id}|' "$p_format")"
assert_eq "C record: tag, then the same join key" "yes" "$(starts_with 'C|#{pane_id}|' "$c_format")"
assert_eq "P record ends with the session name" "#{session_name}" "${p_format##*|}"
assert_eq "C record ends with the pane path" "#{pane_current_path}" "${c_format##*|}"
assert_eq "P record still carries the pid, as data" "yes" "$(contains '|#{pane_pid}|' "$p_format")"

echo
echo "target resolution unit tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
