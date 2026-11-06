# Shared tmux-resurrect directory resolution for Workbench's sidecars.
# The caller supplies wb_tmux so an explicitly selected server stays isolated.
workbench_resurrect_dir() {
  if [ -n "${TMUX_RESURRECT_DIR:-}" ]; then
    printf '%s\n' "$TMUX_RESURRECT_DIR"
    return
  fi
  wb_resurrect_dir=$(wb_tmux show-option -gqv @resurrect-dir 2>/dev/null || true)
  if [ -z "$wb_resurrect_dir" ]; then
    if [ -d "$HOME/.tmux/resurrect" ]; then
      wb_resurrect_dir=$HOME/.tmux/resurrect
    else
      wb_resurrect_dir=${XDG_DATA_HOME:-$HOME/.local/share}/tmux/resurrect
    fi
  fi
  wb_resurrect_host=$(hostname 2>/dev/null || true)
  printf '%s\n' "$wb_resurrect_dir" | sed "s,\$HOME,$HOME,g; s,\$HOSTNAME,$wb_resurrect_host,g; s,~,$HOME,g"
}
