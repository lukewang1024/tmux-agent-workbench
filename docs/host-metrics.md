# tmux host metrics

The resource capsule has three selectable modes. Compact shows CPU and memory,
Standard adds disk usage, and Full adds upload and download speed. Below the
theme's 80-column threshold the capsule is hidden; the selected mode is kept
when it becomes visible again. Each icon is followed by one space, and each
metric is separated from the next by one space; values have no extra left
padding, so the capsule stays compact.
Rates use decimal MB/s (1 MB = 1,000,000 bytes), with two decimals. Rates above
999.99 MB/s display `>999MB/s` without widening the field.

The sampler ships in tmux-agent-workbench; no dotfiles bootstrap is needed.
Run `tmux-agent-workbench host-metrics [compact|standard|full|cpu]` to sample it
directly. Click the resource capsule to choose Compact, Standard or Full, or to
open btop. The selection is stored in the tmux server option
`@workbench-host-metrics-mode` and defaults to `compact`.
The plugin registers it in the optional tmux-adaptive-theme host capsule by default.
Set `@workbench-host-metrics off` and reload the plugin to hide it.
`@workbench-host-metrics-min-width` (80) and
`@workbench-host-metrics-full-min-width` (140) are retained for compatibility;
mode selection is now explicit rather than width-dependent.
Without tmux-adaptive-theme the sampler remains available through the CLI;
the plugin does not replace a custom status-right template.
It is invoked through `sh`, including on Termux where `/bin/sh` may not exist.
No Python, background service or additional monitoring package is required.

- Linux: CPU counter deltas from `/proc/stat`; memory is total minus available.
- macOS: CPU from `top`; memory is active + wired + compressor pages from `vm_stat`.
- Disk: usage of the filesystem containing HOME (`df -Pk`). Override the path
  with `TMUX_METRICS_DISK_PATH` if another mounted filesystem matters more.
- Network: Linux uses byte-counter deltas for the default-route interface from
  `/proc/net/dev`. A new interface or reset counters start a fresh sample. macOS
  uses a one-second `nettop` TCP/UDP delta across external interfaces, excluding
  loopback, because some drivers leave `netstat` receive counters frozen.
  Upload and download are shown separately.
- Termux: uses the Linux path; Android may deny `/proc/stat` or network counters.
  Unavailable metrics show `--`, while available memory/disk metrics still work.
  Real-device Termux acceptance remains pending.

Samples share a two-second cache in `$XDG_CACHE_HOME/tmux-agent-workbench/host-metrics` (default
`~/.cache/tmux-agent-workbench/host-metrics`). The first counter sample and samples more than
120 seconds apart show `--` until a new baseline is established. The tmux
status interval controls subsequent refreshes. No traffic payload is collected.

Icons use Nerd Fonts: `md-cpu-64-bit`, `md-memory`, `md-harddisk`, `md-upload`,
`md-download`, verified against the official glyph map:
https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json

Verification: `python3 -m unittest discover -s test -p 'test_host_metrics*.py'`.
Fixtures cover Linux, macOS, restricted Android counters, caching, interface
changes and counter resets. macOS and Linux also have live sampling coverage.
