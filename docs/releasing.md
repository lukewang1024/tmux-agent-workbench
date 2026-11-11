# PR checks and release delivery

Run `sh tests/p0.sh` for the required fast gate. It checks Rust library and
binary unit tests, CLI startup, task/workspace isolation, pane routing, and
daemon shutdown. Automatic PR and main-push CI runs this P0 suite on Ubuntu,
with a five-minute job timeout; the stable CI gate requires only P0.

Run `sh tests/verify.sh` locally before shipping for the full P1 regression
suite: formatting, all Rust tests, isolated tmux integration, relay pairing,
and the installer. Manual CI dispatch runs this suite on Ubuntu and Apple
Silicon macOS, plus the P2 iSH static-target and Intel title-hook checks.
The release workflow continues to run the full native checks.
Linux and Apple Silicon macOS CI build tmux 3.7c with its official SHA-256.
Ubuntu 24.04's bundled tmux 3.4 lacks the existing menu mouse flags, while
Homebrew may already ship a newer menu implementation. The macOS source
build uses Homebrew libevent, ncurses, utf8proc and jemalloc. Intel title-hook
coverage also checks the current Homebrew tmux.
The stable aggregate check is named **CI gate**; a failed, skipped, or cancelled
platform prevents it from passing.

`main` requires a PR, an up-to-date branch, and CI gate, including for admins.
There is no mandatory reviewer count for this personal repository. Enable
squash auto-merge on the PR (`gh pr merge --auto --squash`); GitHub merges only
once branch protection is satisfied. Update a stale branch and rerun checks.

The local code-ship policy is `auto-merge` with provider `github-auto`. The
machine-local provider creates/reuses a PR, enables native GitHub auto-merge,
and waits for its result. It never bypasses protection or pushes to main.

To release a new version, change `Cargo.toml` and `Cargo.lock` in a PR.
After its merge, successful **push** CI on the current main commit triggers
the release workflow. It creates `v<package version>` only when that tag does
not exist. CI on PR heads, failed runs, and superseded main commits cannot tag.
An ordinary PR without a version change does not create a release.

The iSH i586 target uses Rust 1.95.0 in CI and release, matching the tested
App Store emulator build; other targets continue to use stable Rust.

The workflow builds all eight Rust targets (including static i586 musl for iSH) and both Windows companion targets,
collects artifacts, then publishes in a single final job. New releases stay
in draft until all twelve archive/checksum files have been uploaded. Failed
builds never become Latest. The build jobs are in the same workflow as tag
creation because a tag pushed with GITHUB_TOKEN does not trigger another
push workflow. GitHub Actions uses a read-only token for PR CI.

## Recover an incomplete release

Wait for current main CI to pass, then dispatch:

```sh
gh workflow run release.yml --ref main -f repair_release_tag=v2.0.0-beta.15
```

Recovery checks that the immutable tag matches its Cargo version and belongs
to main's history. Every binary is built from that exact tagged commit. Only
`tests/core-selection.sh` is read from the CI-validated main commit, allowing
repair of the missing host-metrics fixture without changing shipped code.
Existing release notes are preserved; artifacts are uploaded after all targets
succeed. No release tag is moved or overwritten.

If Ubuntu's default archives cannot be reached, the P0 gate and Linux release
jobs retry their test dependencies using signed Ubuntu mirrors. Package
signature verification remains enabled; the binaries are still built from the
original release tag.

The main protection and repository auto-merge setting live in GitHub. The
required status check is `CI gate`, strict/up-to-date is enabled, administrators
are included, and force pushes and branch deletion are disabled.
