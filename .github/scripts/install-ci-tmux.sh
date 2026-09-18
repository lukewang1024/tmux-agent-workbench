#!/bin/sh
# Ubuntu 24.04 ships tmux 3.4, without the menus' display-menu -M support.
set -eu
ci_tmux_root=$(mktemp -d "${RUNNER_TEMP:-/tmp}/workbench-tmux.XXXXXX")
trap 'rm -rf "$ci_tmux_root"' 0
curl -fsSL https://github.com/tmux/tmux/releases/download/3.7c/tmux-3.7c.tar.gz \
  -o "$ci_tmux_root/tmux.tar.gz"
printf '%s  %s\n' 7c60cae9a0e25288e2e24750aafc9e8800fc7fd4555e447e1b29ee4201cfb3bf \
  "$ci_tmux_root/tmux.tar.gz" | sha256sum -c -
tar -xzf "$ci_tmux_root/tmux.tar.gz" -C "$ci_tmux_root"
cd "$ci_tmux_root/tmux-3.7c"
ci_tmux_prefix=${RUNNER_TEMP:?}/workbench-tmux-install
./configure --prefix="$ci_tmux_prefix"
make -j2
make install
printf '%s\n' "$ci_tmux_prefix/bin" >> "$GITHUB_PATH"
"$ci_tmux_prefix/bin/tmux" -V
