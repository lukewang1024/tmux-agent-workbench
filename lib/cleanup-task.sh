# shellcheck shell=sh
# Validate every checkout before removing any. Never delete scratch files or
# a repository's main checkout, even when --force was explicitly requested.
workbench_check_task_cleanup() (
  cleanup_root=$1
  cleanup_force=$2
  cleanup_keep_root=${3:-0}
  for cleanup_member in "$cleanup_root"/* "$cleanup_root"/.[!.]* "$cleanup_root"/..?*; do
    [ -e "$cleanup_member" ] || [ -L "$cleanup_member" ] || continue
    if [ "$cleanup_keep_root" = 1 ] && [ "$cleanup_member" = "$cleanup_root/tasks" ] && [ ! -L "$cleanup_member" ]; then
      continue
    fi
    if [ -L "$cleanup_member" ] || [ ! -f "$cleanup_member/.git" ]; then
      echo "workbench: task retained; non-worktree files remain: $cleanup_member" >&2
      exit 1
    fi
    cleanup_top=$(git -C "$cleanup_member" rev-parse --show-toplevel) || exit 1
    cleanup_member=$(CDPATH='' cd "$cleanup_member" && pwd -P) || exit 1
    [ "$cleanup_top" = "$cleanup_member" ] || exit 1
    cleanup_main=$(git -C "$cleanup_member" worktree list --porcelain | sed -n '1s/^worktree //p')
    [ "$cleanup_main" != "$cleanup_member" ] || {
      echo "workbench: refusing to remove a main checkout: $cleanup_member" >&2; exit 1
    }
    if [ "$cleanup_force" = 0 ] && [ -n "$(git -C "$cleanup_member" status --porcelain --untracked-files=all)" ]; then
      echo "workbench: task retained; uncommitted changes in $cleanup_member" >&2
      exit 1
    fi
  done
)

workbench_clean_task() (
  cleanup_root=$1
  cleanup_force=$2
  cleanup_keep_root=${3:-0}
  workbench_check_task_cleanup "$cleanup_root" "$cleanup_force" "$cleanup_keep_root" || exit 1
  for cleanup_member in "$cleanup_root"/*; do
    [ -f "$cleanup_member/.git" ] || continue
    cleanup_main=$(git -C "$cleanup_member" worktree list --porcelain | sed -n '1s/^worktree //p')
    if [ "$cleanup_force" = 1 ]; then git -C "$cleanup_main" worktree remove --force "$cleanup_member"
    else git -C "$cleanup_main" worktree remove "$cleanup_member"
    fi
  done
  [ "$cleanup_keep_root" = 1 ] || rmdir "$cleanup_root"
)
