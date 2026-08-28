# Attention v2 acceptance audit

Last updated: 2026-08-27

This matrix audits the accepted requirements in
[attention-v2.md](attention-v2.md) against current source and executable
evidence. `attention-v2-status.md` is the compact progress checkpoint; this
file is the requirement-level record used for final acceptance.

## Architecture and protocol

| Requirement | Evidence | Status |
|---|---|---|
| One binary and the accepted public command tree | `src/main.rs`; clap help build/test | proven |
| One detached daemon per tmux server, singleton and server lifecycle | `src/main.rs::ensure_daemon`, `src/daemon.rs`, server-scoped lock/socket; isolated plugin-bootstrap, duplicate-ensure, and server-shutdown tests in `tests/integration-tmux.sh` | proven |
| Upgrade handshake prevents overlapping notifiers | raw versioned IPC envelope in `src/ipc.rs::exchange`; `ensure_daemon` requests `daemon.stop` using the peer protocol and waits for socket removal before spawn | proven |
| UI disconnected/retry; picker fails without daemon and has no scan fallback | `src/sidebar.rs` retry loop; `src/picker.rs` begins with `snapshot.get` and propagates connection failure | proven |
| Protocol-v1 methods and schema-v1 additive JSON | `src/daemon.rs`, `src/ipc.rs`, `src/model.rs`; protocol mismatch test and `tests/golden/snapshot-v1.json` | proven |
| XDG paths, private runtime, per-server names, mode 0600 socket/locks | `src/paths.rs`, `src/daemon.rs`, `src/layout.rs`, `src/relay.rs`; owner/symlink checks and store-mode test | proven |
| Strict config/manifests and atomic reload retention | serde unknown-field rejection, semantic validators; bad config and bad manifest reloads in `tests/integration-tmux.sh` | proven |

## Detection and attention semantics

| Requirement | Evidence | Status |
|---|---|---|
| Scan every non-sidebar pane; shell is not an Agent; four supported Agents | `src/tmux.rs`, `src/process.rs`; exact-alias false-positive test and real compiled `codex` fixture integration | proven |
| Pane + PID/start/executable instance identity and TTL-only metadata hints | `src/state_machine.rs`, `src/detection.rs`; process replacement and metadata validation tests | proven |
| 1 s process, 500 ms active, 2 s idle, 100 ms directed confirmation; 200 lines/64 KiB live capture | scheduler and bounds in `src/detection.rs`, `src/tmux.rs`, and strict config defaults/tests | proven |
| Manifest subset, local whole-Agent replacement, no downloads | `src/manifest.rs`, bundled `manifests/*.toml`; matcher/region/version/alias collision tests | proven |
| working/blocked/idle/unknown and stable working-to-idle done | `src/state_machine.rs`; stabilization, stale grace, initial-state, and persistent-unseen-done tests | proven |
| Exit tombstone, 24 h TTL, seen deletion, no restart persistence | in-memory state machine and tombstone tests, including visible immediate prune | proven |
| Visibility in any attached client acknowledges without changing blocked | `src/tmux.rs::visible_panes`, state visibility test | proven |
| Reason-change/re-entry attention; invalid attention is removed | evidence hash remains in-memory only; reason-change and stale-event removal tests | proven |
| Rollup and next-attention ordering | `src/model.rs`, `src/state_machine.rs`; priority and same-kind oldest-first tests | proven |

## UI, layout, and navigation

| Requirement | Evidence | Status |
|---|---|---|
| Responsive sidebar, legacy options, drag memory, 64-column cap | `src/layout.rs`; isolated hide/restore and exact-width integration | proven |
| Bootstrap existing windows, remember manual off, heal exits, close sole sidebar window | `ensure-all`/disabled markers and hooks; isolated integration assertions | proven |
| Frame-free server-wide session/agent sections, contextual menus, safe metadata, attention-first stable tmux order, visible overflow footer | `src/sidebar.rs`; ordering/orphan/menu-target/footer logic tests and live capture inspection | proven |
| Keyboard/mouse navigation without selection-as-seen | crossterm event loop; acknowledgement occurs only after actual focus observation or tombstone activation | proven |
| Session picker includes empty sessions and restores target; Agent picker targets exact pane/tombstone | `src/picker.rs`; fake-fzf + real tmux control-client/session/Agent integration | proven |
| Five overrideable tracked bindings and sessionist replacement | `workbench.tmux`, `lib/bind-tracked.sh`; live and isolated key-table checks | proven |
| Refresh uses only socket; UI count cannot instantiate or accelerate Detector | single daemon-owned `Detector`; fake Unix-daemon `snapshot.get` refresh contract test | proven |

## Notifications, sound, and relay

| Requirement | Evidence | Status |
|---|---|---|
| Daemon-only 1 s recheck, focus suppression, done/request sound difference, dedupe | `src/notification.rs`; positive timing/dedupe, visible done, and visible blocked tests | proven |
| Original licensed sounds and OpenPeon active pack/volume/mutes/no-repeat | synthesized embedded WAVs in `build.rs`, `assets/SOUNDS-LICENSE.md`, pack traversal/no-repeat tests; platform volume argv | proven |
| macOS overlay/system and Linux notify-send action with structured focus | `assets/macos-overlay.js`, `src/notification.rs`; fixed argv and validated target types | proven |
| Authenticated CESP-only relay, bounds, token rate/burst, dedupe | `src/relay.rs`; real loopback HTTP 401/202/dedupe/413 tests and validation/rate tests | proven |
| Pair/revoke/rotate, independent remotes, mode 0600, no SSH config edits | relay store/commands; `tests/relay-pairing.sh` fake-SSH two-XDG round trip | proven |
| 60 s in-memory exponential retry | `RelaySender`/`PendingOutbound` and bounded backoff test | proven |
| Reverse focus fixed argv, exact pane, session fallback, expired notice | validated mapping; real tmux control-client integration; platform expired-target notification | proven |
| Relay doctor prints and probes RemoteForward | remote hidden `relay probe` invoked through the paired SSH alias | proven |

## Installation, migration, release, and final gates

| Requirement | Evidence | Status |
|---|---|---|
| Release-first/source-fallback POSIX installer and macOS atomic signing | `install`, `tests/install.sh`; installed Mach-O execution verified locally | proven |
| macOS/Linux x86_64/aarch64 release matrix | `.github/workflows/release.yml`; current standard runner labels checked against GitHub's hosted-runner reference | proven in source; actual tagged CI awaits a release tag |
| Old sidebar refusal and actionable doctor | `src/layout.rs`, `src/doctor.rs`, README migration section | proven |
| Authorized dotfiles cut-over and no Agent config mutation | dotfiles diff; old TPM/hooks/options/installers removed, content-matched live-hook cleanup, orphan watcher cleanup; Agent configs contain no old sidebar hooks | proven |
| Full local release-style suite | 56 Rust tests; `tests/integration-tmux.sh`, `tests/relay-pairing.sh`, and `tests/install.sh`; shell syntax and workflow-YAML validation | proven |
| Live release install/reload and runtime smoke | installed binary signature/version, daemon pid 58561 protocol v1, schema-v1 snapshot, clean doctor, three connected frame-free sidebar captures, high-contrast message styles, and no legacy hooks/processes | proven |

The explicit out-of-scope list remains absent: no distributed aggregation,
remote manifest download, SSH config editing, Agent hook installer, independent
error/limit events, mobile push, TTS, pack marketplace/downloader, or retired
rich sidebar surfaces.
