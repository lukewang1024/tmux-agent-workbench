#!/bin/sh
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
generator="$dir/../git/bin/gen-project-configs"
tmuxinator_generator="$dir/../git/bin/gen-tmuxinator-configs"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/wb-project-config.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

export XDG_CONFIG_HOME="$tmp/config"
export WORKBENCH_CODE_ROOT="$tmp/code"
export WORKBENCH_WORKSPACE_ROOT="$tmp/workspace"
mkdir -p "$WORKBENCH_CODE_ROOT/web-app" "$WORKBENCH_WORKSPACE_ROOT"
repo="$WORKBENCH_CODE_ROOT/web-app"
(cd "$repo" && git init -q)
printf '{"scripts":{"dev":"vite"}}\n' > "$repo/package.json"
: > "$repo/pnpm-lock.yaml"

"$generator" "$repo" >/dev/null
config="$XDG_CONFIG_HOME/tmux-agent-workbench/projects/web-app.conf"
[ -f "$config" ]
grep -qxF 'layout=even-vertical' "$config"
grep -qxF 'dev_command=pnpm run dev' "$config"

# Existing metadata is user-owned by default.
sed 's/^dev_command=.*/dev_command=pnpm run storybook/' "$config" > "$config.next"
mv "$config.next" "$config"
"$generator" "$repo" >/dev/null
grep -qxF 'dev_command=pnpm run storybook' "$config"

# --force deliberately replaces the manual edit with a fresh guess.
"$generator" --force "$repo" >/dev/null
grep -qxF 'dev_command=pnpm run dev' "$config"

# With no explicit paths, configured Code/Workspace roots are scanned.
rm "$config"
"$generator" >/dev/null
grep -qxF 'dev_command=pnpm run dev' "$config"

# tmuxinator generation consumes the metadata rather than guessing again.
sed 's/^dev_command=.*/dev_command=pnpm run preview/' "$config" > "$config.next"
mv "$config.next" "$config"
"$tmuxinator_generator" "$WORKBENCH_CODE_ROOT" "$WORKBENCH_WORKSPACE_ROOT" >/dev/null
generated="$XDG_CONFIG_HOME/tmuxinator/web-app.yml"
grep -qxF "            - 'pnpm run preview'" "$generated"

printf 'PASS: project config generation, preservation, force, and consumption\n'
