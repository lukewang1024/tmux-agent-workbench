# Delivery and release

- Run `sh tests/verify.sh` before shipping. This is the same unit, tmux,
  relay, and installer validation required by PR CI on Linux and macOS.
- Ship through a feature branch and PR; enable GitHub squash auto-merge and
  wait for required `CI gate` checks. Never push directly to `main` or bypass
  branch protection. Update from `main` if the strict checks require it.
- For a new release, update both `Cargo.toml` and `Cargo.lock` in the PR.
  After merge, successful main CI automatically tags the version and builds
  all release assets. The release becomes public only after all targets pass.
- Published tags are immutable. Recover a failed release with the release
  workflow's `repair_release_tag` input after current main CI succeeds.
  Recovery builds the original tagged source, using only the validated
  installer fixture from main. Do not move tags or force-push branches.
- See `docs/releasing.md` for repository settings and recovery details.

# Shared agent state

- Change foreground-session state in `StateMachine`, through the shared
  `Tracked::bind_session` transition. Window titles, sidebar cards, notifications
  and checkpoints project the same `AgentSnapshot`; presentation code must not
  infer or maintain a separate foreground session.
- When adding session-owned data, include its reset behavior in that transition
  and its persistence behavior in checkpoint recovery. A UUID prefix becoming a
  full UUID is the same session and must retain the new task's data.
- Changes to shared session data require `tests/session-projections.py` coverage
  across the real tmux window, rendered sidebar, snapshot and checkpoint. Cover
  both hook/redraw orders and daemon recovery; a window-only assertion is
  insufficient. This contract runs in the required `tests/verify.sh` checks.
