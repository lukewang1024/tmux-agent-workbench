# PR checks and release delivery

Run `sh tests/p0.sh` for the required fast gate. It checks Rust library and
binary unit tests, CLI startup, task/workspace isolation, pane routing, and
daemon shutdown. Automatic PR and main-push CI runs this P0 suite on Ubuntu,
with a five-minute job timeout; the stable CI gate requires only P0.

Run `sh tests/verify.sh` locally before shipping for the full P1 regression
suite: formatting, all Rust tests, isolated tmux integration, relay pairing,
and the installer. Manual CI dispatch runs this suite on Ubuntu and Apple
Silicon macOS, plus the P2 iSH static-target and Intel title-hook checks.
Stable releases run the full native release checks; prereleases use the fast
target set below.
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

| Selection | Required targets | Optional targets |
| --- | --- | --- |
| Standard release | Termux arm64 (`aarch64-linux-android`), Linux x64 (`x86_64-unknown-linux-gnu`), Apple Silicon macOS (`aarch64-apple-darwin`) | iSH i586 (`i586-unknown-linux-musl`) when its compatibility code changes |
| `windows-companion/` changed | Windows x64 client | Windows ARM64 client on a full release |
| Full release | The same three standard targets | Intel macOS, Linux musl x64, iSH i586, Linux ARM64, Android x64, and Windows ARM64 when its client code changed |

The release workflow compares the tagged source with the previous release.
Rust artifacts run only when Rust source, Cargo metadata, or files packaged
with the Rust binary changed. Windows builds run only when
`windows-companion/` changed. Full releases expand the architecture range for
the affected component; they do not force unrelated components to build.
Standard targets and a changed Windows x64 client are required. Extra targets
run at low priority: their failures do not block publication, and only their
successful archives are included. Windows ARM64 starts after Windows x64 has
completed.

The iSH i586 binary serves iSH's 32-bit x86 Linux environment on iOS; iSH
provides that environment through user-mode x86 emulation ([iSH project](https://github.com/ish-app/ish)).
It is not a typical Linux desktop architecture. Full releases run the macOS
tmux integration suite. At most twelve archives/checksum files are published;
the final set can be smaller when low-priority builds fail or the corresponding
component did not change.

The iSH i586 target uses Rust 1.95.0 in CI and release, matching the tested
App Store emulator build; other targets continue to use stable Rust.

The workflow collects successful artifacts, then publishes in a single final
job after all required targets succeed. Failed required builds never publish;
failed low-priority targets are simply omitted. Stable releases become Latest
after required targets pass. The build jobs are in the same workflow as tag
creation because a tag pushed with GITHUB_TOKEN does not trigger another push
workflow. GitHub Actions uses a read-only token for PR CI.

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

If Ubuntu's default archives cannot be reached, the P0 gate, Linux P1 and
release jobs retry their test dependencies using signed Ubuntu mirrors. Package
signature verification remains enabled; the binaries are still built from the
original release tag.

The main protection and repository auto-merge setting live in GitHub. The
required status check is `CI gate`, strict/up-to-date is enabled, administrators
are included, and force pushes and branch deletion are disabled.
