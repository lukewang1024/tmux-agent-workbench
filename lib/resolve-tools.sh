# resolve-tools.sh — shared tool resolution, sourced (not executed) by
# bin/mux-inspect and git/bin/gen-tmuxinator-configs so the "what git TUI /
# what editor" logic exists exactly once.

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
