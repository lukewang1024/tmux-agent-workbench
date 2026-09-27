#!/usr/bin/env bash
# Hermetic unit tests for Grok session detection / extraction.
#
# Unlike test/run-tests.sh (Docker, real CLI binaries), these tests need no
# grok binary and no tmux: they source the library functions directly and
# drive them against a fixture ~/.grok/active_sessions.json via GROK_HOME.
# Run locally with:  bash test/grok-unit-tests.sh   (or: just test-grok)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# --- isolate all side effects into a temp sandbox ---
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT INT TERM
export GROK_HOME="$SANDBOX/.grok"
export TMUX_RESURRECT_DIR="$SANDBOX/resurrect"
export TMUX_ASSISTANT_RESURRECT_DIR="$SANDBOX/state"
mkdir -p "$GROK_HOME" "$TMUX_RESURRECT_DIR" "$TMUX_ASSISTANT_RESURRECT_DIR"

# Fixture: two live sessions sharing the SAME cwd (the case cwd-scoped lookups
# get wrong) plus one in another cwd. Keyed by PID, so grok resolves each
# correctly.
cat >"$GROK_HOME/active_sessions.json" <<'JSON'
[
  { "session_id": "019f1897-89a9-7a40-baa4-587f80e772c0", "pid": 1001, "cwd": "/work/notion", "opened_at": "2026-06-30T12:57:31.562612Z" },
  { "session_id": "019f18aa-bd4b-70b3-862e-5b8771880ca4", "pid": 1002, "cwd": "/work/notion", "opened_at": "2026-06-30T13:14:31.060946Z" },
  { "session_id": "019eff47-9b97-7681-9d9c-ca48cc5e2a2a", "pid": 1003, "cwd": "/work/xai",    "opened_at": "2026-06-30T13:08:41.652726Z" }
]
JSON

# Source the libraries (save script has a main() guard, so this only defines
# functions; the top-level mkdir/log lines land in the sandbox via the env
# overrides above).
# shellcheck source=../scripts/lib-detect.sh
source "$REPO_DIR/scripts/lib-detect.sh"
# shellcheck source=../scripts/save-assistant-sessions.sh
source "$REPO_DIR/scripts/save-assistant-sessions.sh" >/dev/null 2>&1

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

echo "== detect_tool =="
assert_eq "bare grok"                 "grok" "$(detect_tool 'grok')"
assert_eq "grok with --resume"        "grok" "$(detect_tool 'grok --resume 019f1897-89a9-7a40-baa4-587f80e772c0')"
assert_eq "grok with abs path"        "grok" "$(detect_tool '/Users/x/.grok/bin/grok --cwd /tmp')"
assert_eq "no false positive grokfoo" ""     "$(detect_tool 'grokfoo bar')"
assert_eq "unrelated binary"          ""     "$(detect_tool 'vim README.md')"

echo "== get_grok_session: PID lookup in registry (primary) =="
assert_eq "pid 1001 -> session A"             "019f1897-89a9-7a40-baa4-587f80e772c0" "$(get_grok_session 1001 'grok')"
assert_eq "pid 1002 same cwd -> session B"    "019f18aa-bd4b-70b3-862e-5b8771880ca4" "$(get_grok_session 1002 'grok')"
assert_eq "pid 1003 other cwd -> session C"   "019eff47-9b97-7681-9d9c-ca48cc5e2a2a" "$(get_grok_session 1003 'grok')"

echo "== get_grok_session: args fallback (chicken-and-egg after restore) =="
assert_eq "unknown pid, --resume <uuid>" "0193abcd-1234-7abc-8def-0123456789ab" "$(get_grok_session 9999 'grok --resume 0193abcd-1234-7abc-8def-0123456789ab')"
assert_eq "unknown pid, --resume=<uuid>" "0193abcd-1234-7abc-8def-0123456789ab" "$(get_grok_session 9999 'grok --resume=0193abcd-1234-7abc-8def-0123456789ab')"
assert_eq "unknown pid, short -r <uuid>"  "0193abcd-1234-7abc-8def-0123456789ab" "$(get_grok_session 9999 'grok -r 0193abcd-1234-7abc-8def-0123456789ab')"
assert_eq "unknown pid, bare grok -> empty" "" "$(get_grok_session 9999 'grok')"
assert_eq "registry wins over args"       "019f1897-89a9-7a40-baa4-587f80e772c0" "$(get_grok_session 1001 'grok --resume 0193abcd-1234-7abc-8def-0123456789ab')"

echo "== extract_cli_args: session flags stripped, others kept =="
assert_eq "strip --resume keep --effort" "--effort high" "$(extract_cli_args grok 'grok --resume 019f1897-89a9-7a40-baa4-587f80e772c0 --effort high')"
assert_eq "strip -c keep --model"        "--model grok-4" "$(extract_cli_args grok 'grok -c --model grok-4')"

echo
echo "grok unit tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
