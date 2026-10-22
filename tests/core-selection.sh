#!/bin/sh
# Regression: installing a release must supersede an older checkout binary.
set -eu
repo=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' 0
trap 'exit 1' HUP INT TERM
plugin="$fixture/plugin with spaces"
export XDG_DATA_HOME="$fixture/data"
export CORE_SELECTION_LOG="$fixture/selected"
mkdir -p "$plugin/bin" "$plugin/lib" "$plugin/target/release" \
  "$XDG_DATA_HOME/tmux-agent-workbench/bin" "$fixture/bin"
cp "$repo/bin/tmux-agent-workbench-cli" "$plugin/bin/"
cp "$repo/bin/workbench-status-popup" "$repo/bin/workbench-menu" \
  "$repo/bin/workbench-menu-mouse-mode" "$repo/bin/wb-responsive" "$plugin/bin/"
cp "$repo/workbench.tmux" "$repo/Cargo.toml" "$plugin/"
cp "$repo/lib/bind-tracked.sh" "$repo/lib/host-metrics.sh" \
  "$repo/lib/resolve-core.sh" "$plugin/lib/"

make_core() {
  # Arguments are expanded by the generated executable.
  # shellcheck disable=SC2016
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" > "$CORE_SELECTION_LOG.args"\nprintf "%%s\\n" "%s"\n' "$2" > "$1"
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
  *'#{socket_path}'*) printf '%s\n' /tmp/core-selection.sock ;;
  *'#{client_session}'*) printf '%s\n' test-session ;;
  *'#{window_id}'*) printf '%s\n' @1 ;;
  *'#{pane_id}'*) printf '%s\n' %1 ;;
  'list-clients -F #{client_name} #{client_width}') printf '/dev/pts/8 %s\n' "${MOCK_CLIENT_WIDTH:-160}" ;;
  'display-popup '*)
    for popup_command do :; done
    sh -c "$popup_command"
    ;;
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
  for kind in host tmux agent; do
    actual=$("$plugin/bin/workbench-menu" "$kind" /dev/pts/8 %1)
    [ "$actual" = "$1" ] || { printf 'wrong %s menu core: %s\n' "$kind" "$actual" >&2; exit 1; }
    [ "$(cat "$CORE_SELECTION_LOG.args")" = "status-menu $kind --pane %1 --client /dev/pts/8" ]
  done
  actual=$("$plugin/bin/workbench-status-popup" agent /dev/pts/8 %1 --focus-agent)
  [ "$actual" = "$1" ] || { echo 'wrong focused agent menu core' >&2; exit 1; }
  [ "$(cat "$CORE_SELECTION_LOG.args")" = 'status-menu agent --pane %1 --client /dev/pts/8 --focus-agent' ]
  actual=$("$plugin/bin/workbench-menu-mouse-mode" /dev/pts/8)
  [ "$actual" = "$1" ] || { echo 'wrong mouse mode core' >&2; exit 1; }
  [ "$(cat "$CORE_SELECTION_LOG.args")" = 'menu-mouse-mode --client /dev/pts/8' ]
  actual=$("$plugin/bin/wb-responsive" @1 /dev/pts/8)
  [ "$actual" = "$1" ] || { echo 'wrong sidebar toggle core' >&2; exit 1; }
  [ "$(cat "$CORE_SELECTION_LOG.args")" = 'sidebar-control toggle @1' ]
  actual=$(MOCK_CLIENT_WIDTH=80 "$plugin/bin/wb-responsive" @1 /dev/pts/8)
  [ "$actual" = "$1" ] || { echo 'wrong sidebar popup core' >&2; exit 1; }
}
check 'tmux-agent-workbench 2.0.0-beta.14' "$installed"
export TMUX_AGENT_WORKBENCH_BIN="$fixture/override"
check 'tmux-agent-workbench override' "$fixture/override"
unset TMUX_AGENT_WORKBENCH_BIN
rm "$installed"
check 'tmux-agent-workbench 2.0.0-beta.12' "$checkout"

# A broken explicit override must not silently launch another version.
export TMUX_AGENT_WORKBENCH_BIN="$fixture/missing-override"
if "$plugin/bin/tmux-agent-workbench-cli" --version >/dev/null 2>&1; then
  echo 'missing override unexpectedly used another core' >&2
  exit 1
fi
if "$plugin/bin/workbench-status-popup" tmux /dev/pts/8 %1 >/dev/null 2>&1; then
  echo 'menu unexpectedly used another core with a missing override' >&2
  exit 1
fi
unset TMUX_AGENT_WORKBENCH_BIN
rm "$checkout"
[ "$("$plugin/bin/workbench-menu-mouse-mode" /dev/pts/8)" = mouse ]
"$plugin/bin/tmux-agent-workbench-cli" --help >/dev/null
printf '%s\n' 'core selection: CLI, plugin, menus, mouse mode and sidebars agree on release, override and fallback'
