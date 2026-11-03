# Delivery and release

- Run `sh tests/p0.sh` for the required PR gate. Run `sh tests/verify.sh`
  before shipping for the full P1 unit, tmux, relay, and installer suite.
  Manual CI dispatch also runs the extended Linux/macOS and platform checks.
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
