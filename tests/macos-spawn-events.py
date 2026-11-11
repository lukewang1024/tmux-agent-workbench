#!/usr/bin/env python3
"""A long PATH must not multiply a quiet daemon's process events on macOS."""
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time

if sys.platform != 'darwin':
    print('SKIP macOS process-event regression')
    raise SystemExit(0)

binary = Path(sys.argv[1]).resolve()
tmux = shutil.which('tmux')
if not tmux:
    raise SystemExit('tmux is required for the process-event regression')
tmux = str(Path(tmux).resolve())

# Keep the socket path short and isolate all workbench state from the user.
with tempfile.TemporaryDirectory(prefix='wb-spawn-', dir='/tmp') as directory:
    root = Path(directory)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color',
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
    for name in ('TMUX', 'TMUX_PANE', 'TMUX_TMPDIR'):
        env.pop(name, None)
    for name in ('XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[name] = str(root / name)
    env['PATH'] = os.pathsep.join(
        [str(root / f'missing-{n}') for n in range(20)]
        + [str(Path(tmux).parent), '/usr/bin', '/bin'])

    daemon = None
    log = root / 'daemon.log'
    try:
        subprocess.run([tmux, '-S', socket, '-f', '/dev/null', 'new-session',
                        '-d', '-s', 'probe', '/bin/sh'], env=env, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=15)
        with log.open('w') as stderr:
            daemon = subprocess.Popen([str(binary), 'daemon', 'run'], env=env,
                                      stdout=subprocess.DEVNULL, stderr=stderr)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if daemon.poll() is not None:
                raise AssertionError(f'daemon exited during startup: {log.read_text()}')
            status = subprocess.run([str(binary), 'daemon', 'status'], env=env,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                    timeout=5)
            if status.returncode == 0:
                break
            time.sleep(.1)
        else:
            raise AssertionError(f'daemon did not become ready: {log.read_text()}')

        time.sleep(1)
        queue = select.kqueue()
        queue.control([select.kevent(daemon.pid, filter=select.KQ_FILTER_PROC,
                                     flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                                     fflags=select.KQ_NOTE_FORK)], 0, 0)
        events = 0
        start = time.monotonic()
        while time.monotonic() - start < 6:
            events += len(queue.control(None, 128, .05))
        queue.close()
        assert daemon.poll() is None, 'daemon exited during the measurement'
        # A healthy quiet server creates a few real tmux/ps children per second.
        # Without PATH resolution, the 20 missing entries turn each into 21.
        assert events < 120, f'quiet daemon generated {events} forks in 6 seconds'
        print(f'PASS quiet daemon generated {events} forks in 6 seconds with a long PATH')
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                subprocess.run([str(binary), 'daemon', 'stop'], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=8)
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=5)
        subprocess.run([tmux, '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=8)
