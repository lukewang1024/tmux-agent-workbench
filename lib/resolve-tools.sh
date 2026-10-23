# shellcheck shell=sh
# Tool resolution for explicitly opened temporary tools.

resolve_git_tool() {
  printf '%s\n' "${WORKBENCH_GIT_TOOL:-tig}"
}

resolve_editor() {
  e=${VISUAL:-${EDITOR:-}}
  if [ -z "$e" ]; then
    if command -v nvim >/dev/null 2>&1; then e=nvim; else e=vim; fi
  fi
  printf '%s\n' "$e"
}
