# PR checks and release delivery

Run `sh tests/verify.sh` locally before shipping. It checks formatting, all
Rust tests, isolated tmux integration, relay pairing, and the installer.
CI repeats the same command on Ubuntu and macOS for every PR and main push.
Linux CI pins tmux 3.7c with its official SHA-256; Ubuntu 24.04's bundled
tmux 3.4 lacks the existing menu mouse flags. macOS uses Homebrew tmux.
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

The workflow builds all seven Rust targets and both Windows companion targets,
collects artifacts, then publishes in a single final job. New releases stay
in draft until all eleven archive/checksum files have been uploaded. Failed
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

The main protection and repository auto-merge setting live in GitHub. The
required status check is `CI gate`, strict/up-to-date is enabled, administrators
are included, and force pushes and branch deletion are disabled.
