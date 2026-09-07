#!/bin/sh
# Regression: installing a release must supersede an older checkout binary.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' 0
trap 'exit 1' HUP INT TERM
plugin="$fixture/plugin with spaces"
export XDG_DATA_HOME="$fixture/data"
export CORE_SELECTION_LOG="$fixture/selected"
mkdir -p "$plugin/bin" "$plugin/lib" "$plugin/target/release" \
  "$XDG_DATA_HOME/tmux-agent-workbench/bin" "$fixture/bin"
cp "$repo/bin/tmux-agent-workbench-cli" "$plugin/bin/"
cp "$repo/workbench.tmux" "$repo/Cargo.toml" "$plugin/"
cp "$repo/lib/bind-tracked.sh" "$plugin/lib/"

make_core() {
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$2" > "$1"
  chmod +x "$1"
}
installed="$XDG_DATA_HOME/tmux-agent-workbench/bin/tmux-agent-workbench-core"
checkout="$plugin/target/release/tmux-agent-workbench"
make_core "$installed" 'tmux-agent-workbench 2.0.0-beta.14'
make_core "$checkout" 'tmux-agent-workbench 2.0.0-beta.12'
make_core "$fixture/override" 'tmux-agent-workbench override'
cat > "$fixture/bin/tmux" <<'SH'
#!/bin/sh
case "$*" in
  'set-option -g @workbench_attention_bin '*) printf '%s\n' "$4" > "$CORE_SELECTION_LOG" ;;
esac
exit 0
SH
chmod +x "$fixture/bin/tmux"
export PATH="$fixture/bin:$PATH"
unset TMUX_AGENT_WORKBENCH_BIN

check() {
  actual=$("$plugin/bin/tmux-agent-workbench-cli" --version)
  [ "$actual" = "$1" ] || { printf 'wrong CLI core: %s\n' "$actual" >&2; exit 1; }
  bash "$plugin/workbench.tmux" > /dev/null
  [ "$(cat "$CORE_SELECTION_LOG")" = "$2" ] || { echo 'wrong plugin core' >&2; exit 1; }
}
check 'tmux-agent-workbench 2.0.0-beta.14' "$installed"
export TMUX_AGENT_WORKBENCH_BIN="$fixture/override"
check 'tmux-agent-workbench override' "$fixture/override"
unset TMUX_AGENT_WORKBENCH_BIN
rm "$installed"
check 'tmux-agent-workbench 2.0.0-beta.12' "$checkout"
printf '%s\n' 'core selection: installed release, explicit override, checkout fallback OK'
