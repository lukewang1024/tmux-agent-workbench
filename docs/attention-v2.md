# Workbench v2: local tmux agent attention

Status: accepted implementation specification (2026-08-27)

This document is the durable source of truth for the v2 implementation. The
existing task/worktree/inspection/handoff features remain in this repository
and continue to work. V2 adds the local attention system; it does not make the
distributed Workbench provider a dependency.

## Confirmed decisions

- Keep the existing Workbench shell commands and git-worktree layer. Add the
  Rust attention binary alongside them.
- The cut-over may update both this repository and the user's dotfiles. Remove
  the old `tmux-agent-sidebar` integration from dotfiles when v2 is ready.
- Follow Peon Ping's platform split: a JXA/Cocoa overlay on macOS and
  `notify-send` on Linux. Windows/WSL are not in the v1 release matrix.
- Generate two original non-voice WAV sounds (done and request) and document
  their license; do not vendor Peon Ping sounds.
- The relay listener is one user-level process per machine. Per-tmux-server
  daemons connect to it; they do not each bind port 19999.
- Manifest `regex` and `line_regex` use Rust's linear-time `regex` syntax.
  Unsupported constructs are rejected by `config check`.

## Scope and architecture

Ship one Rust executable named `tmux-agent-workbench` with these commands:

```
daemon ensure|status|stop
snapshot [--json]
sidebar
pick session|agent
attention next
agent explain <pane> [--show-content]
metadata report
config check
reload
doctor
relay serve|pair|revoke|rotate|doctor
```

There is one daemon for each tmux server. Plugin sourcing starts it
idempotently and it exits after its tmux socket disappears. It remains alive
when all UI clients are closed. A stable server-scoped Unix socket and
single-instance lock prevent duplicate daemons. Upgrade handoff must perform a
protocol handshake and stop the old process before the new process may send
notifications. Sidebar reconnects and renders `disconnected` when IPC is down;
pickers fail non-zero and never scan tmux locally.

Unix IPC is newline-delimited JSON request/response with
`protocol_version = 1`. It exposes at least `snapshot.get`, `daemon.status`,
`config.reload`, `agent.explain`, `attention.ack`, and `metadata.report`.

The stable JSON snapshot has `schema_version = 1` and includes server identity,
generation, observation time, session/window/pane tmux targets, opaque agent
instance id, kind, label, base and display state, reason category, attention
event id/kind/seen/since, stale/visibility, manifest version/rule id, and exited
agent tombstones. V1 may only gain optional fields; breaking changes increment
the schema version.

Paths:

- config: `$XDG_CONFIG_HOME/tmux-agent-workbench/config.toml`
- manifest overrides:
  `$XDG_CONFIG_HOME/tmux-agent-workbench/manifests/<agent>.toml`
- logs/state: `$XDG_STATE_HOME/tmux-agent-workbench/`
- socket/lock: `$XDG_RUNTIME_DIR/tmux-agent-workbench/`, falling back to a
  user-isolated mode-0700 temporary directory
- read-only OpenPeon packs: `~/.openpeon/packs/`, configurable

`config check` rejects unknown keys and invalid values. Reload parses and
validates a complete replacement configuration plus manifests before swapping
it atomically; a failure retains the previous valid state. Built-in manifests
ship with the executable. A local agent manifest replaces its complete built-in
manifest. V1 never downloads manifests.

## Detection and state

Scan every non-sidebar pane in the current tmux server, regardless of how it
was launched. Fully support Codex, Claude, Trae, and OpenCode. A normal shell is
not an agent. Identity is pane plus foreground-process fingerprint: PID,
process start time, and executable identity. Hook session ids are metadata only.

Authority is ordered as follows:

1. foreground process tree proves that an agent exists;
2. live bottom pane content, pane title, and bundled/user manifests determine
   state;
3. launcher options and metadata reports may supply kind, label, session id,
   and reason hints only when accompanied by a TTL.

Discovery runs every second. Working, blocked, and unknown panes are captured
every 500 ms; idle panes back off to two seconds. tmux hooks and metadata
reports wake the daemon immediately. Capture only the bottom 200 live-buffer
lines and at most 64 KiB; never read a client's scroll viewport.

Manifests are a small Herdr-inspired subset with version, minimum engine
version, aliases, priority, state, region, `contains`, `regex`, `line_regex`,
`all`, `any`, `not`, visible idle/blocker/working, and skip-state-update.
Regions include whole recent content, top/bottom lines, bottom non-empty lines,
prompt box, areas after the last prompt or matching rule, and pane title.

Base state is exactly `working | blocked | idle | unknown`. A recognized agent
with no matching rule is unknown. Strong blocked/working signals publish
immediately. A working-to-idle transition gets a targeted 100 ms recapture and
requires three confirmations, bounded by 700 ms. One process/capture failure
retains the last state and marks it stale; after three seconds of continuous
failure publish unknown.

`done` is a display/attention state, never a base state. Stable working-to-idle
creates done. An active process disappearing creates a done tombstone. Initial
idle and unknown-to-idle do not create done.

An agent is seen when its pane is active and visible in any attached client.
Done created in an already-visible pane is immediately seen. Seeing blocked
acknowledges its queue event without changing the blocked base state. Re-entry
to blocked or a changed reason fingerprint creates a new attention event. The
fingerprint persists category and rule id only; evidence content is hashed in
memory and prompt text is never persisted. Unseen exit tombstones last 24
hours; seen tombstones are removed immediately; tombstones do not survive a
daemon restart. Initial blocked after restart creates attention and notifies.

Session rollup order is blocked, done, working, idle, unknown. Next-attention
orders unacknowledged blocked before done, then oldest first.

## Sidebar, pickers, and tmux integration

Retain the old pane/layout behavior, reimplemented here: each sufficiently wide
window receives a sidebar, default left width 26, main area minimum 80, hidden
when narrow and restored later, draggable remembered width capped at 64. Honor
the existing `@sidebar_width`, `@sidebar_position`, `@sidebar_auto_create`,
minimum/maximum/main-minimum options. Removed rich-UI options have no effect.

Sidebar has two server-wide sections: every tmux session (including sessions
without agents) followed by every detected agent. Rows contain only a state
glyph, label, counts/state, and blocked reason category. Label precedence is
explicit metadata/user label, tmux window name, then kind plus pane id.
Unacknowledged attention sorts first; everything else follows stable tmux
session/window/pane order. Support wheel, j/k, arrows, Enter, and click; keep
selection visible and show hidden row count. Enter/click navigates directly;
moving selection does not mark seen. Right-click or `m` opens contextual
session/agent actions, while the footer exposes new-session and global picker,
next-attention, and reload actions.

Use `fzf-tmux` popups. Session picker lists all sessions with rollup and agent /
attention counts and restores the session's last active window/pane. Agent
picker lists all agents with attention first. Picking a live agent goes to its
pane. Picking a tombstone shows a metadata-only completion summary and then
acknowledges it.

Default tracked bindings:

- `prefix+Tab`: current-window sidebar
- `prefix+BTab`: all-window sidebars
- `prefix+g`: session picker, replacing sessionist goto
- `prefix+a`: agent picker
- `prefix+n`: next attention

Every key is overrideable through `@workbench-key-*` and uses the existing
tracked-binding mechanism so reloads remove stale bindings safely.

## Notification and sound policy

Only the daemon emits notifications, one second after a transition and only
after confirming the event remains valid. Done maps to CESP `task.complete`;
blocked maps to `input.required`. V1 does not emit independent `task.error` or
`resource.limit` events; those are reason categories.

Suppress desktop notifications whenever the target is visible in any client.
Play done sound only in the background. Play one request sound for a newly
blocked event even when visible. Focus acknowledges attention without changing
the base state.

Sound and desktop notification are enabled by default. Bundle original,
project-licensed done/request WAVs. Support OpenPeon `openpeon.json` packs,
active pack, volume, category mute, and no-repeat. Do not implement pack
registry/download, TTS, or mobile delivery.

On macOS default to a neutral Peon-Ping-style JXA/Cocoa overlay and allow system
notification mode. On Linux use `notify-send`. Overlay content is limited to
agent/session/reason. Click actions pass structured targets to this binary and
never execute event-provided shell. Local and relayed macOS click-to-focus must
select the exact tmux pane. On Linux use a notification action when supported;
otherwise display only.

## Relay

The user-level relay defaults to `127.0.0.1:19999`. Its only event endpoint is
authenticated `POST /v1/events`, accepting only CESP `task.complete` and
`input.required` plus allowlisted focus metadata. Bearer token is always
required, including explicit non-loopback binds. Limit payloads to 16 KiB and
each token to 60 events/minute with burst 10. Validate length, character set,
and target type of every tmux/SSH field.

Multiple remotes may pair to one laptop. Each SSH host gets an independent
token, remote id, and focus mapping. `relay pair <ssh-host>` creates and stores
a mode-0600 token locally, performs one SSH invocation of the remote binary to
write its XDG config, and stores remote id to SSH config host alias. It never
edits SSH config. Doctor prints a `RemoteForward 19999 localhost:19999` example
and verifies the tunnel. Revoke removes one pairing. Rotate updates both ends
atomically.

Remote send failures retry by event id with exponential backoff in memory for
at most 60 seconds, then log and drop. Laptop deduplicates event ids. Relayed
clicks use the paired SSH alias and validated socket/pane id to execute a fixed
focus subcommand argv. If pane is gone but session exists, switch to session;
if both are gone, report target expired. Never execute arbitrary remote input.

## Install, migration, and release

CI publishes macOS and Linux x86_64/aarch64 binaries. Installer downloads a
release first and may fall back to a Cargo source build. Plugin loading checks
binary/repository versions and tells the user when an upgrade is needed; it
does not overwrite user config.

If old `tmux-agent-sidebar` is also loaded, refuse to create a second sidebar.
Doctor explains how to remove the old TPM entry and hooks. The actual cut-over
updates dotfiles to remove the old TPM entry, hook installer/updater/bootstrap
references, and obsolete options while installing/loading the new binary.
Do not commit to the old sidebar repository. Layout, snapshot, and notification
ideas may be independently reimplemented in this repository.

V1 does not modify Claude, Codex, Trae, or OpenCode configuration. It only
offers TTL-bound `metadata report`.

## Verification contract

Automated tests must cover:

- four-agent fixtures for working, blocked, idle, unknown, false positives,
  and changed TUI versions;
- state stabilization, three-second stale grace, process replacement, direct
  exit tombstone, 24-hour TTL, and reason-change attention;
- multiple-client visibility, seen/blocked acknowledgement, rollup, and
  next-attention ordering;
- isolated tmux servers: singleton/lifecycle/IPC version/restart, no-agent
  sessions, responsive sidebar layout, and picker targets;
- proof that UI refresh uses only IPC, never process scan, `capture-pane`, or a
  notification backend;
- fake notifications: one-second recheck, focus suppression, done/request
  sound policy, and deduplication;
- relay auth, rate/burst limit, payload bound, injection rejection,
  pair/revoke/rotate, 60-second retry, and expired focus target;
- failed config/manifest reload retaining the last valid state;
- schema-v1 snapshot golden files;
- macOS/Linux x86_64/aarch64 release matrix.

Core dependencies are tmux >= 3.2 and `fzf-tmux`; doctor reports missing or
incompatible dependencies precisely. Blocked/working must appear in the
canonical snapshot within one second under normal sampling. A notification
that survives its one-second recheck must be delivered within two seconds of
the transition. Multiple sidebar/picker clients must not increase daemon scan
or capture frequency.

Explicitly out of scope: Distributed Workbench aggregation/provider, remote
manifest downloads, automatic SSH config edits, agent-hook installers,
independent task-error/resource-limit events, mobile push, TTS, sound-pack
market/download, and all removed rich-sidebar features (prompt/tool/subagent,
git, activity log, pet).
