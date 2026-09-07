# Background resource usage

IPC requests and responses are serialized into a complete newline-delimited
message before writing to the socket. This avoids a syscall for each JSON
fragment while preserving the existing wire format.

Agent discovery refreshes process identity, ancestry, executable and command line.
It excludes kernel threads and CPU, memory and disk counters. Command lines remain
available for agents launched through Node, Bun or Deno. Exited processes are
removed on the next scan.

Sidebars request snapshots every second while visible and every five seconds
while hidden, detached or obscured by a zoomed pane. Visibility is checked at most
once per five seconds in the background worker. Focusing or interacting with a
hidden sidebar triggers an immediate refresh; otherwise a newly visible sidebar
resumes within the next background refresh. Popups keep the foreground cadence.
Hidden sidebars also reduce input polling and sorting-preference reads.

Validation:

- `cargo test --lib` covers JSON framing and batching, process identity and exit
  removal, thread exclusion, and visibility including zoom and unfocused panes.
- `sh tests/integration-tmux.sh` exercises the existing tmux integration contract.
- `python3 tests/sidebar-refresh.py target/release/tmux-agent-workbench` measures
  snapshot requests in an isolated detached session, then attaches a control-mode
  client and verifies that the sidebar resumes foreground refresh.
