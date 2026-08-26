---
name: handoff
description: Hand an in-progress coding task to another configured coding-agent profile in tmux. Use when the user asks to transfer, hand off, or continue the current task in another agent, model, or thinking level, or when this agent must proactively continue through another profile because its plan or availability is running out.
---

# Handoff

Transfer the entire active task; do not narrow it to the unfinished step that happens to be visible now.

1. If the user named an exact profile, use it exactly. If they named only an
   agent family or gave no target, run `mux-handoff profiles --json` and choose
   a concrete profile yourself based on the remaining task. Do not ask merely
   because several profiles match, and do not silently substitute another
   profile after choosing one.
2. Inspect the current worktree and task state. Write a compact free-text
   summary covering the objective, completed work, current state, verification
   already performed, next actions, blockers/risks, and any source transcript
   path or session ID you know. Mention temporary environment or pane-attached
   processes when the successor may need to recreate them.
3. Make the handoff the final task action. Pass the summary on stdin with a
   quoted heredoc so shell syntax in the summary cannot execute:

   ```sh
   mux-handoff --target <exact-profile> <<'HANDOFF_SUMMARY'
   <summary>
   HANDOFF_SUMMARY
   ```

4. On success, stop editing and only report that delivery succeeded. The
   command starts the successor immediately and schedules this pane to close
   after its grace period. Do not wait for the successor to acknowledge or
   prove understanding.
5. On failure, remain in the current task, inspect the returned error, and
   either repair the invocation or choose another profile yourself unless the
   user explicitly required the failed profile.

The target shares the current filesystem and working directory. The mechanism
does not migrate transient process environment or processes attached to this
pane. A user may stop the automatic close during the grace period with
`mux-handoff cancel`.
