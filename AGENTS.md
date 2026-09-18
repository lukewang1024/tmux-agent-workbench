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
