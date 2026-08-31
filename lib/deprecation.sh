# shellcheck shell=sh
# POSIX sh. Emit a compatibility warning on every direct legacy invocation.
[ "${WB_SHIM_CALL:-0}" = 1 ] || {
  wb_deprecation_name=$(basename "$0")
  case $wb_deprecation_name in
    mux-agent) wb_deprecation_replacement='tmux-agent-workbench agent start' ;;
    mux-handoff) wb_deprecation_replacement='tmux-agent-workbench agent handoff' ;;
    mux-inspect) wb_deprecation_replacement='tmux-agent-workbench inspect' ;;
    mux-run-task) wb_deprecation_replacement='tmux-agent-workbench run' ;;
    workbench-session-pick) wb_deprecation_replacement='tmux-agent-workbench pick session' ;;
    ws-add) wb_deprecation_replacement='tmux-agent-workbench add' ;;
    ws-done) wb_deprecation_replacement='tmux-agent-workbench done' ;;
    ws-new) wb_deprecation_replacement='tmux-agent-workbench new' ;;
    ws-promote) wb_deprecation_replacement='tmux-agent-workbench promote' ;;
    *) wb_deprecation_replacement='tmux-agent-workbench' ;;
  esac
  # shellcheck disable=SC2016 # backticks are literal user-facing punctuation
  printf '%s is deprecated; use `%s` (compatibility retained through 2.1)\n' \
    "$wb_deprecation_name" "$wb_deprecation_replacement" >&2
  unset wb_deprecation_name wb_deprecation_replacement
}
