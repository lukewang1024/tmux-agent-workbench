# ADR: v2 Beta client-aware attention and unified workflow

Status: accepted, 2026-08-29

This ADR supersedes the focus, persistence, mobile, notification routing, and
public CLI conclusions in `attention-v2.md`. That document remains the history
and baseline for hook-first lifecycle detection. The target release is
`2.0.0-beta`.

## Decisions

- `workbenchd` remains the sole writer for Agent lifecycle, semantic events,
  attention, seen state, endpoint routing, and server-lifetime checkpoints.
- Process identity proves an Agent exists. Hooks are lifecycle authority.
  Screen evidence remains a low-confidence estimate and process exit remains
  the no-Stop completion fallback.
- Base state remains `working | blocked | idle | unknown`; `done` is display
  state and exit is represented by a tombstone.
- Runtime attention and seen counters are monotonic. Event ids combine a
  random runtime id with a sequence and are opaque to clients.
- Seen requires an exact active pane, affirmative terminal focus, and no
  Workbench overlay, or an explicit acknowledgement. Unknown focus is unseen.
- Canonical semantic events are `task.complete`, `input.required`,
  `task.error`, and `session.start`. Transport acceptance never means seen.
- A focused viewer suppresses desktop notification and marks queued attention
  seen, but complete/input/error still sound on the viewing endpoint when
  possible. Otherwise the router tries one endpoint at a time in descending
  activity order. Complete/input expire after five minutes and errors after
  one minute, measured from original event creation across daemon restarts.
- `client-protocol-v1` is four-byte big-endian length-prefixed UTF-8 JSON with
  a 1 MiB frame bound. SSH authenticates bootstrap. A random, one-use,
  one-minute attachment token binds the independent control and PTY channels.
- Device UUIDs are stable client data. Endpoint and attachment ids are per
  connection. Snapshot-v1 exposes only safe client presence metadata.
- Clipboard RPC is explicit UTF-8 text only, rejects NUL, and is limited to
  1 MiB. There is no background synchronization.
- Wide clients use the shared pane sidebar. Narrow clients use a per-client
  fullscreen popup and temporarily suppress the shared sidebar only for the
  window they view.
- `tmux-agent-workbench` is the public CLI, explicitly distinct from Distributed
  Workbench's `workbench` CLI. The former short name `wb`, legacy `mux-*`,
  `ws-*`, and relay surfaces remain as every-invocation-warning shims through
  2.1 and may be removed in 2.2.
- Durable workspace TOML records live below
  `$XDG_DATA_HOME/tmux-agent-workbench/workspaces`. UUID is identity and
  canonical root is the dedupe key; names are ambiguous display aliases.
  Migration never moves or recreates worktrees. `tmux-agent-workbench done` updates a record only
  after the corresponding resource is actually removed.
- Windows uses the Rust core and a minimal C# companion only where native app
  notification activation requires it. Setup is always explicit.
- Theme and dotfiles enable Beta features by machine-readable capabilities,
  never by parsing a version string.

## Security and persistence

Checkpoint files are atomic mode-0600 files below
`$XDG_STATE_HOME/tmux-agent-workbench/servers`. Reconciliation requires the
same tmux server incarnation, a live pane, and the same process fingerprint.
Prompt or screen evidence, clipboard contents, route credentials, attachment
tokens, and channel secrets are never persisted.

Click payloads contain only opaque event ids. The server resolves and validates
the current incarnation and live target; it falls back from pane to session and
finally to a metadata-only summary.

## Compatibility

Configuration precedence is environment, new `tmux-agent-workbench` keys, legacy keys, then
defaults. User configuration is not rewritten implicitly. Unknown fields are
preserved by explicit migration tooling and reported diagnostically.

Beta activation capabilities include `client-protocol-v1`,
`status-fragments-v1`, and `responsive-popup-v1`.
