"""The Sidebar overlay leaves the live tmux status line visible and stable."""
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

from daemon_fixture import running_daemon
from terminal_screen import screen_rows

binary = str(Path(sys.argv[1]).resolve())


def check(position):
    with tempfile.TemporaryDirectory(prefix='wb-sidebar-status-', dir='/tmp') as root:
        socket = root + '/tmux.sock'
        env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_LANGUAGE='en',
                   TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
        for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
            env.pop(key, None)
        for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR', 'XDG_DATA_HOME'):
            env[key] = root + '/' + key
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
        client = popup = None

        def tmux(*args):
            return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).rstrip('\n')

        def drain(seconds=.05):
            data = b''
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], max(0, until - time.monotonic()))[0]:
                    data += os.read(master, 65536)
            return data

        try:
            tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'test', '-x', '80', '-y', '30', 'sleep 120')
            pane = tmux('display-message', '-p', '-t', 'test', '#{pane_id}')
            for option, value in [('status', 'on'), ('status-position', position), ('status-left-length', '60'),
                                  ('status-left', '#[fg=red,bg=green]STATUS0'), ('status-right', '')]:
                tmux('set-option', '-g', option, value)
            client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'test'], env=env,
                                      stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            with running_daemon(binary, env):
                output = drain(.5)
                client_name = tmux('list-clients', '-F', '#{client_name}').splitlines()[0]
                popup = subprocess.Popen([binary, 'sidebar', '--popup', '--pane', pane, '--client', client_name],
                                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                deadline = time.monotonic() + 10
                while b'Agents' not in output:
                    assert popup.poll() is None and time.monotonic() < deadline, 'Sidebar did not open'
                    output += drain()
                if position == 'top':
                    # Preserve the existing full-screen overlay for top status.
                    assert popup.poll() is None
                    print('PASS top-status Sidebar overlay opens', flush=True)
                    return
                row = 29
                for index in range(1, 4):
                    marker = f'STATUS{index}'
                    tmux('set-option', '-g', 'status-left', f'#[fg=red,bg=green]{marker}')
                    tmux('refresh-client', '-S', '-t', client_name)
                    deadline = time.monotonic() + 5
                    while marker not in screen_rows(output, 80, 30)[row]:
                        assert popup.poll() is None and time.monotonic() < deadline, (position, marker, screen_rows(output, 80, 30))
                        output += drain()
                assert popup.poll() is None
                print(f'PASS {position} tmux status stays live while the Sidebar is open', flush=True)
        finally:
            subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, capture_output=True, timeout=5)
            os.close(master)
            os.close(slave)
            for process in (popup, client):
                if process is not None:
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


check('bottom')
check('top')
