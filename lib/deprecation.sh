# POSIX sh. Emit a compatibility warning once per command through 2.1.
[ "${WB_SHIM_CALL:-0}" = 1 ] || {
  wb_deprecation_dir=${XDG_STATE_HOME:-$HOME/.local/state}/tmux-agent-workbench/deprecations
  wb_deprecation_name=$(basename "$0")
  wb_deprecation_file=$wb_deprecation_dir/$wb_deprecation_name
  if [ ! -f "$wb_deprecation_file" ]; then
    mkdir -p "$wb_deprecation_dir"
    chmod 700 "$wb_deprecation_dir" 2>/dev/null || true
    printf '%s is deprecated; use wb (compatibility retained through 2.1)\n' "$wb_deprecation_name" >&2
    (umask 077 && : >"$wb_deprecation_file")
  fi
  unset wb_deprecation_dir wb_deprecation_name wb_deprecation_file
}
