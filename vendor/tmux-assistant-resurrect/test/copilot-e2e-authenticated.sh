#!/usr/bin/env bash
# Authenticated end-to-end proof: does a real Copilot conversation actually come
# back after save -> kill -> restore?
#
# This exists because nothing else can answer that question. Every other layer
# runs unauthenticated, where Copilot creates a session directory and lock but
# never makes the session resumable -- so a lookup that returns an unresumable
# UUID looks identical to a correct one. That blind spot shipped a bug: the save
# hook stored sessions whose restore printed "No session, task, or name matched"
# into the user's pane. Run this after touching Copilot session discovery.
#
#   GH_TOKEN=$(gh auth token) just test-copilot-e2e
#
# Costs a small number of Copilot AI credits. Not wired into CI: fork pull
# requests cannot read repository secrets.
#
# Structured in two halves. Copilot's TUI does not accept
# tmux send-keys without an attached client, so the conversation is seeded and
# interrogated non-interactively; the save -> kill -> restore cycle in between
# runs against a live interactive TUI through the real hooks.
set -uo pipefail

if [ -z "${GH_TOKEN:-}${GITHUB_TOKEN:-}${COPILOT_GITHUB_TOKEN:-}" ]; then
	echo "SKIP: no GitHub token in the environment (GH_TOKEN / GITHUB_TOKEN /"
	echo "      COPILOT_GITHUB_TOKEN). This test needs a real Copilot login."
	exit 0
fi
if ! command -v copilot >/dev/null 2>&1; then
	echo "SKIP: copilot binary not installed"
	exit 0
fi

REPO="${REPO_DIR:-$HOME/tmux-assistant-resurrect}"
export COPILOT_AUTO_UPDATE=false
MAGIC="BANANAPHONE7"
UUID="1c0f11a7-0000-4000-8000-$(printf %012x "$$")"

step() { printf '\n=== %s ===\n' "$1"; }
pane() { tmux capture-pane -t e2e -p | grep -vE '^\s*$' | tail -"${1:-12}"; }

step "1. seed a real conversation (non-interactive, fixed session id)"
copilot --no-auto-update --allow-all --session-id="$UUID" \
	-p "Remember this magic word for later: $MAGIC. Reply with just: STORED" </dev/null 2>&1 | head -3
DIR="$HOME/.copilot/session-state/$UUID"
echo "session dir: $(ls "$DIR" 2>/dev/null | tr '\n' ' ')"
[ -f "$DIR/session.db" ] && echo "PASS: session.db written -> session is resumable" || {
	echo "FAIL: no session.db"; exit 1; }

step "2. open that session in a live TUI (what a user would have running)"
tmux new-session -d -s e2e -c /tmp
tmux send-keys -t e2e "copilot --no-auto-update --allow-all --resume=$UUID" Enter
for _ in $(seq 1 60); do
	lock=$(find "$DIR" -name 'inuse.*.lock' 2>/dev/null | head -1)
	[ -n "$lock" ] && break
	sleep 1
done
[ -n "${lock:-}" ] || { echo "FAIL: resumed TUI never took the lock"; pane 20; exit 1; }
echo "PASS: live TUI holds lock $(basename "$lock") on the resumed session"

step "3. save hook"
bash "$REPO/scripts/save-assistant-sessions.sh"
SAVED="$HOME/.local/share/tmux/resurrect/assistant-sessions.json"
[ -f "$SAVED" ] || SAVED="$HOME/.tmux/resurrect/assistant-sessions.json"
jq -c '.sessions[] | select(.tool=="copilot")' "$SAVED"
SAVED_SID=$(jq -r '.sessions[] | select(.tool=="copilot") | .session_id' "$SAVED")
[ "$SAVED_SID" = "$UUID" ] && echo "PASS: saved the live session's UUID" || {
	echo "FAIL: saved [$SAVED_SID] != [$UUID]"; exit 1; }

step "4. kill it (simulating a reboot)"
pkill -f 'copilot-linux' 2>/dev/null
sleep 5

step "5. restore hook"
bash "$REPO/scripts/restore-assistant-sessions.sh"
sleep 20
if tmux capture-pane -t e2e -p | grep -qi "No session, task, or name matched"; then
	echo "FAIL: restore replayed a command Copilot rejected"; pane 15; exit 1
fi
if pgrep -f "copilot.*$UUID" >/dev/null || find "$DIR" -name 'inuse.*.lock' | grep -q .; then
	echo "PASS: restored Copilot is running on the same session, no resume error"
else
	echo "FAIL: no live Copilot after restore"; pane 15; exit 1
fi
pane 8

step "6. THE PROOF: does the conversation still remember?"
pkill -f 'copilot-linux' 2>/dev/null
sleep 4
ANSWER=$(copilot --no-auto-update --allow-all --resume="$UUID" \
	-p "What magic word did I ask you to remember earlier? Reply with only that word." </dev/null 2>&1)
echo "$ANSWER" | head -5
if echo "$ANSWER" | grep -q "$MAGIC"; then
	echo ""
	echo "PASS: the resumed conversation recalled $MAGIC -- history survived the cycle"
else
	echo ""
	echo "FAIL: resumed session did not recall the magic word"
	exit 1
fi

tmux kill-session -t e2e 2>/dev/null
