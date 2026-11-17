#!/usr/bin/env python3
"""Verify plugin startup ordering on a real tmux server without sessions."""
import os
from pathlib import Path
import subprocess
import tempfile
import time

repo = Path(__file__).resolve().parents[1]
source = (repo / 'workbench.tmux').read_text()
version_check = source.index('  if [ -n "$expected_version" ]')
startup_begin = source.index('  fi\n', version_check) + len('  fi\n')
startup = source[startup_begin:source.index("  tmux set-hook -g 'after-new-window[921]'")]
with tempfile.TemporaryDirectory(prefix='wb-daemon-startup-') as directory:
    root = Path(directory)
    socket = str(root / 'tmux.sock')
    log = root / 'starts'
    core = root / 'fake core'
    core.write_text('#!/bin/sh\nset -eu\n[ "$*" = "daemon ensure" ]\n'
                    'if tmux has-session 2>/dev/null; then\n'
                    '  printf "ready\\n" >> "$STARTUP_LOG"\n'
                    'else\n  printf "premature\\n" >> "$STARTUP_LOG"\nfi\n')
    core.chmod(0o755)
    script = root / 'startup.sh'
    script.write_text('set -eu\nATTENTION_BIN=$STARTUP_CORE\n' + startup)
    config = root / 'tmux.conf'
    config.write_text('set -g exit-empty off\nset -g default-shell /bin/sh\n')
    env = dict(os.environ, STARTUP_LOG=str(log), STARTUP_CORE=str(core))
    env.pop('TMUX', None)
    env.pop('TMUX_PANE', None)

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True)

    def load():
        tmux('run-shell', f'bash "{script}"')

    def starts():
        return log.read_text().splitlines() if log.exists() else []

    def wait_starts(count):
        deadline = time.monotonic() + 5
        while len(starts()) < count and time.monotonic() < deadline:
            time.sleep(.02)
        assert starts() == ['ready'] * count, starts()

    try:
        tmux('-f', str(config), 'start-server')
        load()
        time.sleep(.2)
        assert starts() == [], starts()
        tmux('new-session', '-d', '-s', 'first')
        wait_starts(1)
        # A configuration reload must recover the daemon immediately.
        load()
        wait_starts(2)
        load()
        wait_starts(3)
        # Reloads replace the indexed hook rather than accumulating callbacks.
        tmux('new-session', '-d', '-s', 'second')
        wait_starts(4)
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print('daemon startup ordering: ok')
