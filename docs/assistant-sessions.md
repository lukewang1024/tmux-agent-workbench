# Assistant session persistence

Workbench coordinates `tmux-resurrect` layouts, assistant session identities,
and `agent-team` pane membership. Its adapters include code adapted from
[tmux-assistant-resurrect](https://github.com/timvw/tmux-assistant-resurrect)
at `f11ca73cc5591546fb20654dbe6378021ff9cd72`, copyright 2026 Tim Van
Wassenhove, under the [MIT license](../LICENSES/tmux-assistant-resurrect-MIT.txt).
The imported compatibility implementation retains its upstream Bash interpreter;
Workbench's entrypoints and orchestration use POSIX `sh` and Python.

Session persistence requires Bash, jq, Python 3, tmux-resurrect and the
appropriate assistant executable. Team restoration also requires an
`agent-team` version containing the recovery fixes from `b553a01`.

| Assistant | Session discovery | Resume |
| --- | --- | --- |
| Claude Code | Workbench hook, legacy PID hook state, or CLI session argument | `claude --resume` |
| GitHub Copilot CLI | Live PID lock and session database, or a resumable CLI session argument | `copilot --resume=` |
| OpenCode | Workbench hook, CLI argument, legacy plugin state, or cwd-scoped SQLite fallback | `opencode -s` |
| Codex CLI | Workbench hook, PID tags, CLI argument, SQLite or rollout fallback | `codex resume` |
| Pi | CLI session argument or cwd-scoped session file | `pi --session` |
| Oh My Pi (`omp`) | CLI session argument, terminal breadcrumb or session file | `omp --resume` |
| Grok | PID registry or CLI session argument | `grok --resume` |
| TraeX | Workbench lifecycle hook or session-peer file | `traex --resume` |

The file/database fallbacks are best-effort when a tool does not supply a
unique live identity. OpenCode's database fallback and Pi's session-file
fallback can choose the wrong conversation when multiple sessions share a
working directory. Copilot saves only resumable sessions with a session
database; a fresh session without content cannot yet be resumed. When process
arguments have lost quoting boundaries, Copilot options with spaces in their
values may be omitted; if an ambiguous option restricts permissions, all
replayed flags are dropped and Copilot's normal prompting defaults apply.
Grok deliberately does not replay saved CLI arguments, because its session
restores its own configuration and positional arguments can be prompts. TraeX
stores its identity in `workbench-trae-sessions.json`; its independent resume
does not currently reconstruct arbitrary original CLI flags. Team members are
resumed by `agent-team`, which owns their context, model and permission settings
and never replays the original prompt.

## Save and restore

Use a **complete tmux-resurrect save**, normally prefix then `Ctrl-s`, to save
the layout and matching assistant and team sidecars. `tmux-agent-workbench
resurrect save` saves assistant metadata only; it does not save a tmux layout
and cannot replace a complete snapshot. `resurrect restore` invokes the
assistant/team restore coordinator after a layout already exists.

The existing `assistant-sessions.json` format, older entries containing only a
`pane` label, legacy tracking state and relaunch vouchers remain readable.
Legacy state files belonging to live processes are copied without moving their
originals, so older in-process trackers can continue writing until they exit.
Sidecars are saved beside the layout using `TMUX_RESURRECT_DIR`, then
`@resurrect-dir`, then an existing `~/.tmux/resurrect`, otherwise
`$XDG_DATA_HOME/tmux/resurrect`. Each distinct new layout snapshot has a private
sidecar archive keyed by its digest and a unique save ID, so even two identical
layouts can restore their own assistant and team metadata. New layouts carry a marker
that prevents assistant restore when a required sidecar or restore inventory
is missing or mismatched. A
mismatched known team is not started as independent assistants. Old snapshots
without a team sidecar cannot reliably reconstruct
team membership; ordinary assistant entries can still be resumed.

If a pane has a team marker but its team state is missing, mismatched or
ambiguous, a complete save fails instead of recording that pane as a standalone
assistant. Restore refuses a new snapshot whose matching archive is missing.
Stopped members are not started. Members with a missing or ambiguous identity
remain unconfirmed. A previously occupied pane is not taken over; a missing
working directory is left at its shell. An in-flight tool call cannot be
continued. Run
`tmux-agent-workbench resurrect status` to inspect saved assistant identities,
and `tmux-agent-workbench resurrect clean` to remove stale legacy hook state.

## Existing options and session-less commands

The options `@assistant-resurrect-capture-env`,
`@assistant-resurrect-relaunch`, `@assistant-resurrect-relaunch-allow-file`,
`@assistant-resurrect-save-timeout`, and the environment overrides
`TMUX_ASSISTANT_RESURRECT_DIR` and `ASSISTANT_RESURRECT_SAVE_TIMEOUT` remain
compatible. Configured environment values are stored in the private sidecar;
logs redact their values. On Linux the adapter can read configured variables
from a live process; that process-environment fallback is unavailable on macOS.
Credential-shaped CLI flags are removed, but credentials embedded inside opaque
argument values cannot be recognized reliably.

Session-less commands still require an exact entry in the existing voucher:

```sh
tmux-agent-workbench resurrect relaunch candidates
tmux-agent-workbench resurrect relaunch seed
tmux-agent-workbench resurrect relaunch add 'claude agents'
```

The candidate ledger is advisory. `add` requires an exact candidate and keeps
the existing confirmation rule for permission, remote and workspace flags.
An empty voucher authorizes nothing. Existing voucher files remain valid.

Workbench's native lifecycle hooks can be installed with
`tmux-agent-workbench hooks install all`. This installs the Claude and OpenCode
session trackers as Workbench-managed hooks as well as the lifecycle reporters;
no separate TPM plugin is required. Existing installations need to rerun that
command to enable the newly integrated trackers.

The former `vendor/tmux-assistant-resurrect` path is a compatibility symlink to
the Workbench-owned module, so previously configured tracker and script paths
continue to resolve. Workbench's own save/restore hooks do not use that path;
there is only one maintained implementation.

## Verification

`sh tests/assistant-sessions.sh` exercises the imported adapter contracts with
mocked assistants and an isolated tmux target test. The full `sh tests/verify.sh`
also exercises team state, installation, and complete snapshot round trips.
Set `WORKBENCH_TEST_RESURRECT_SCRIPTS` to a tmux-resurrect `scripts` directory
and `WORKBENCH_TEST_AGENT_TEAM_CLI` to the agent-team executable to include the
real layout and team coordination paths. Assistant processes remain fixtures;
authenticated model calls are not part of these tests.
