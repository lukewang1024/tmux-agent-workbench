# Attention v2 implementation status

Last updated: 2026-08-29

## 2.0.0-beta client-aware extension (2026-08-29)

The superseding decisions are recorded in
[`v2-beta-client-attention.md`](v2-beta-client-attention.md). Implemented in
the Beta tree:

- additive snapshot client metadata plus attention/seen sequence fields;
- strict length-prefixed `client-protocol-v1`, SSH control/PTTY attach binding,
  stable device ids, heartbeat/offline/detach grace, clipboard bounds, and
  daemon-owned client registry;
- exact tmux tty/client binding and conservative focus/overlay-aware seen;
- canonical semantic categories, endpoint activity ranking, transport
  acceptance dedupe, retry/failover queue, five-minute/one-minute expiry, and
  legacy local/relay fallback;
- mode-0600 atomic server-incarnation checkpoints with pending delivery
  recovery and no prompt, screen, clipboard, or credential persistence;
- `tmux-agent-workbench` workflow dispatcher, argv-safe
  `tmux-agent-workbench run`, managed opt-in for
  `tmux-agent-workbench agent start`, every-invocation legacy shim warnings, and an atomic TOML workspace
  registry with lazy non-destructive migration;
- narrow-client fullscreen popup, popup-first attention ordering, shared
  sidebar suppression/restoration, and four unstyled responsive status
  fragments selected and colored by `tmux-adaptive-theme`;
- explicit Termux controls and optional API detection, WSL interop paths, and
  an unpackaged Windows App SDK companion with x64/arm64 release jobs.

Automated macOS verification covers Rust unit/golden tests, isolated tmux,
relay, installer, legacy workflow suites, POSIX syntax, and a release build.
Real Termux notification activation/clipboard and real Windows/WSL toast and
Windows Terminal activation remain the explicit pre-release device checklist;
they are not represented as locally verified.

The accepted specification is [attention-v2.md](attention-v2.md). This file is
an implementation checkpoint so work can resume safely after context compaction.

## Complete

- Hook-first lifecycle authority for Claude, Codex, TraeX, and OpenCode, with
  process exit as the authoritative no-Stop completion fallback.
- `hook ingest` / `agent.event.report`, event deduplication, ordering and
  foreground-session fencing, process/pane/session identity checks, and
  per-tmux-server 30-second spool replay.
- Additive snapshot-v1 source/confidence/estimate/hook-health fields. Screen
  evidence is advisory only, is dimmed in the sidebar, and cannot create
  blocked/done attention or notifications.
- Explicit idempotent `hooks install|check|remove` management for Claude JSON,
  Codex TOML, TraeX JSON, and the OpenCode XDG plugin. Foreign hooks and unknown
  configuration survive install/remove; malformed input fails atomically.
- Native `session.start` and `task.error` CESP delivery, including Claude
  `PostToolUseFailure`, OpenCode `session.error`, and in-memory failure
  classification for Codex/TraeX `PostToolUse`. These signals do not create
  false lifecycle transitions; session starts never create attention and tool
  errors leave the Agent working while delivering the error category.

- Rust crate and `tmux-agent-workbench` command tree.
- Schema-v1 snapshot and agent/attention/process/tmux target models.
- Strict, unknown-key-rejecting config with documented defaults and bounds.
- Version-1 manifest model using Rust `regex` validation.
- XDG config/state/runtime paths, mode-0700 runtime directory, and a short
  user-isolated `/tmp` fallback for macOS Unix socket length limits.
- Stable per-tmux-server identity and socket/lock names.
- Protocol-v1 JSON IPC request/response and client errors.
- Per-server daemon lock, mode-0600 Unix socket, lifecycle tied to the tmux
  socket, status/snapshot/reload/stop methods, upgrade stop-before-start
  handshake, idempotent `daemon ensure`, and process-session detachment so the
  daemon survives the originating tmux `run-shell` job.
- Pure attention state machine covering initial idle, initial blocked,
  working-to-idle three-sample stabilization (700 ms bound), reason-change
  attention, visibility acknowledgement, three-second stale grace, direct-exit
  tombstones, 24-hour pruning, explicit acknowledgement, stable ordering, and
  next-attention priority.
- Unseen `done` remains the display state across subsequent idle captures until
  focus/ack; blocked/done events are removed as soon as their underlying state
  stops being valid, so stale attention cannot remain in the queue.
- Real tmux inventory across all sessions/windows, sidebar-pane exclusion,
  all-client visible-pane collection, validated pane targets, and bottom-only
  capture capped at 200 lines / 64 KiB.
- Cross-platform process-tree discovery using PID ancestry, process start time,
  executable identity, and exact aliases for Codex/Claude/Trae/OpenCode.
- Four bundled version-1 manifests, local whole-agent override loading,
  priority ordering, safe matcher evaluation, and required content regions,
  including real after-last-horizontal-rule slicing. Duplicate rule ids,
  unsafe categories, and cross-Agent alias collisions are rejected.
- Daemon-owned detection scheduler: process scan every second, active capture
  every 500 ms, idle capture every two seconds, and 100 ms directed idle
  confirmation. Canonical snapshot generation changes only when agent data
  changes.
- Snapshot session summaries include every tmux session (including no-Agent
  sessions), rollup, counts, and last-active window/pane for socket-only UI.
- `agent explain`, `attention next/ack`, and bounded TTL `metadata report` IPC
  plus CLI flows. Pane content is absent unless `--show-content` is explicit.
- One-second notification recheck/dedupe scheduler, focus suppression policy,
  macOS overlay/system and Linux `notify-send` backends, safe structured macOS
  overlay click-to-focus, and original generated/embedded done/request WAVs.
- Interactive socket-only sidebar with keyboard/mouse/viewport behavior,
  session and Agent `fzf-tmux` pickers, tombstone summaries, and structured
  focus.
- Rust sidebar layout controller with legacy option compatibility, responsive
  hide/restore, draggable width memory, duplicate healing, and old-sidebar
  refusal. A sole remaining sidebar closes its window; the UI uses the tmux
  pane border directly rather than drawing a nested frame. Plugin source starts
  the daemon silently and installs all five tracked bindings and layout hooks.
- Existing windows are bootstrapped idempotently on plugin load. Explicit
  toggle-off is remembered separately from responsive hiding, while an
  unexpected sidebar exit is healed. Attention-first sorting preserves numeric
  tmux inventory order, and overflow reserves a visible hidden-row footer.
- Doctor implementation and release-first/Cargo-fallback POSIX installer.
- OpenPeon pack/active-pack/volume/category-mute/no-repeat sound selection with
  path traversal rejection and embedded sound fallback.
- Authenticated user-level relay receiver with 16 KiB bounds, strict CESP and
  focus metadata validation, per-token rate limiting, event deduplication,
  pairing/revocation/rotation state, fixed SSH argv, and mode-0600 storage.
- Outbound relay delivery uses event-id deduplication and bounded exponential
  retry; remote notification focus uses validated, fixed SSH/tmux argv with the
  session fallback required by the v1 protocol.
- Relay doctor probes the configured reverse tunnel from the remote. Remote
  focus selects the most recently active attached client explicitly (SSH has no
  implicit tmux client), and expired targets produce a desktop-visible notice.
- Cross-platform release CI and snapshot-v1 golden coverage. The POSIX
  installer stages and atomically replaces binaries and ad-hoc signs the staged
  Mach-O on macOS.
- Authorized dotfiles cut-over: the legacy sidebar TPM entry, agent hooks,
  updater, resize hook, and resurrect wrappers are no longer installed; v2
  loads after sessionist so its configurable prefix+g binding wins.
- The dotfiles live-server migration now removes retired hooks by inspecting
  their command content even after the old binary option is already absent;
  it does not blindly clear shared hook indices.

## Verified

- `cargo test`: 75 tests passing (74 library tests plus 1 binary test).
- Isolated real tmux server smoke test: daemon start, status, schema-v1 snapshot,
  second ensure singleton behavior, stop, and socket cleanup.
- Isolated real tmux server with a compiled `codex` fixture: discovered from
  process ancestry and published as an unknown Agent in generation 1.
- Full isolated plugin load: five key bindings, daemon, new-window auto-create,
  `@pane_role=sidebar`, and the responsive default width.
- Isolated tmux integration suite: singleton, protocol/schema, failed reload
  retention, responsive sidebar hide/restore, sole-sidebar window closure, and
  daemon shutdown with the server lifecycle.
- Installer smoke test, including source-build fallback.
- Relay integration covers real HTTP auth/delivery/dedupe/payload bounds and a
  fake-SSH pair/rotate/revoke round trip across independent local/remote XDG
  stores. Isolated tmux coverage exercises exact remote pane focus and expired
  pane fallback to its session through an attached control client.
- Live-server verification: daemon connected after plugin reload, all sidebar
  panes show frame-free snapshot content, and transient tmux messages/prompts
  use a high-contrast semantic warning background.

## Acceptance

- The requirement-level audit is recorded in
  [attention-v2-audit.md](attention-v2-audit.md).
- The final release-style suite passed: Rust unit/golden tests, isolated tmux,
  relay pairing, installer, POSIX/Bash syntax, and release workflow YAML.
- The signed release build was installed and reloaded into the live tmux
  server; daemon status, schema-v1 snapshot, doctor, sidebar captures, message
  styles, and legacy-hook absence were verified.
