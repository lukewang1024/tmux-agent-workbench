# bind-tracked.sh — sourced (not executed) by workbench.tmux and
# git/workbench-git.tmux.
#
# `tmux bind-key` only ever registers the key you name — it never notices
# that a *previous* load bound a *different* key to the same command. Reload
# the config after changing an @workbench-key-* option (or its default
# changes between versions) and the old key keeps silently working as a
# ghost binding alongside the new one. bind_tracked remembers the key it
# last bound (in a private @workbench-_bound-* option) and unbinds it first
# if the resolved key has changed since.
#
# usage: bind_tracked <option> <state-option> <default-key> <description> <command...>
bind_tracked() {
  opt="$1"; state_opt="$2"; default="$3"; desc="$4"; shift 4
  key="$(tmux show-option -gqv "$opt" 2>/dev/null || true)"
  key="${key:-$default}"
  prev="$(tmux show-option -gqv "$state_opt" 2>/dev/null || true)"
  if [ -n "$prev" ] && [ "$prev" != "$key" ]; then
    desc_opt="${state_opt}-description"
    owner_desc="$(tmux show-option -gqv "$desc_opt" 2>/dev/null || true)"
    owner_desc="${owner_desc:-$desc}"
    live_desc=$(tmux list-keys -T prefix -N 2>/dev/null | awk -v key="$prev" '
      $2 == key { $1 = ""; $2 = ""; sub(/^[[:space:]]+/, ""); print; exit }
    ')
    if [ "$live_desc" = "$owner_desc" ]; then
      tmux unbind-key -T prefix "$prev" 2>/dev/null || true
    fi
  fi
  # An empty default means "available, but unbound by default". This lets the
  # sidebar own low-frequency actions without removing the override option.
  if [ -z "$key" ]; then
    tmux set-option -gu "$state_opt" 2>/dev/null || true
    tmux set-option -gu "${state_opt}-description" 2>/dev/null || true
    return 0
  fi
  tmux set-option -g "$state_opt" "$key"
  tmux set-option -g "${state_opt}-description" "$desc"
  tmux bind-key -N "$desc" "$key" "$@"
}
