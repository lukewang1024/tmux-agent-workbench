#!/usr/bin/env python3
"""Exercise session Sidebar routing and keyboard prefix through an isolated real tmux client."""
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time

repo = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='wb-prefix-') as directory:
    socket = directory + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color')
    env.pop('TMUX', None)
    env.pop('TMUX_PANE', None)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
    process = None

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, cwd=directory, text=True).strip()

    def drain():
        deadline = time.monotonic() + .12
        while time.monotonic() < deadline:
            if select.select([master], [], [], .01)[0]:
                os.read(master, 65536)

    def event(button, x, y, release=False):
        os.write(master, f'\033[<{button};{x};{y}{"m" if release else "M"}'.encode())
        drain()

    def table(expected):
        actual = tmux('list-clients', '-F', '#{client_key_table}')
        assert actual == expected, (actual, expected)

    def click(x, y):
        event(0, x, y)
        event(0, x, y, True)

    def prefix():
        os.write(master, b'\x02')
        drain()
        table('prefix')

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'test', '/bin/sh')
        tmux('set', '-g', 'default-shell', '/bin/sh')
        tmux('set', '-g', 'mouse', 'on')
        tmux('set', '-g', 'status-left-length', '30')
        tmux('set', '-g', 'status-left', '#[range=user|wb_sidebar]SESSION#[range=] #[range=user|wb_tmux]MENU#[range=]')
        tmux('set', '-g', 'status-right', '')
        tmux('bind', '-T', 'root', 'MouseDown2Status', 'set', '-g', '@middle', 'yes')
        # Reload must replace the previous broad binding, not just avoid adding it.
        tmux('bind', '-T', 'prefix', 'Any', 'switch-client', '-T', 'prefix')
        first = tmux('display', '-p', '#{pane_id}')
        second = tmux('split-window', '-h', '-d', '-P', '-F', '#{pane_id}')
        tmux('set', '-s', 'command-alias[927]', "wb-status-route=if-shell -F '#{==:#{mouse_status_range},wb_sidebar}' 'set -g @sidebar yes' 'set -g @menu yes'")
        env['TMUX'] = socket + ',0,0'
        source = (repo / 'workbench.tmux').read_text()
        bindings = source[source.index('# Unbound mouse motion'):source.index('# If the optional theme')]
        script = Path(directory) / 'bindings.sh'
        script.write_text(bindings)
        subprocess.run(['/bin/sh', str(script)], env=env, check=True)
        subprocess.run(['/bin/sh', str(script)], env=env, check=True)
        process = subprocess.Popen(['tmux', '-S', socket, 'attach', '-t', 'test'], stdin=slave, stdout=slave, stderr=slave, env=env)
        drain()
        drain()
        click(3, 30)
        table('root')
        assert tmux('show', '-gqv', '@sidebar') == 'yes'
        prefix()
        for x, y in ((1, 30), (4, 30), (1, 10), (5, 10), (41, 10), (60, 10), (20, 30)):
            event(35, x, y)
            assert tmux('list-clients', '-F', '#{client_key_table}') == 'prefix', (x, y)
        tmux('set', '-gu', '@sidebar')
        click(3, 30)
        table('root')
        assert tmux('show', '-gqv', '@sidebar') == 'yes'
        prefix()
        click(60, 10)
        table('root')
        assert tmux('display', '-p', '#{pane_id}') == second
        prefix()
        click(10, 30)
        table('root')
        assert tmux('show', '-gqv', '@menu') == 'yes'
        prefix()
        event(1, 10, 30)
        event(1, 10, 30, True)
        table('root')
        assert tmux('show', '-gqv', '@middle') == 'yes'
        prefix()
        tmux('bind', '-T', 'prefix', 'x', 'set', '-g', '@keyboard', 'yes')
        os.write(master, b'x')
        drain()
        table('root')
        assert tmux('show', '-gqv', '@keyboard') == 'yes'
        # q is a bound command; N is unbound. Both must leave prefix, while
        # pointer movement above retains it. The following x must reach the
        # pane rather than invoking the prefix binding.
        tmux('set', '-g', 'display-panes-time', '100')
        for key in (b'q', b'N'):
            tmux('set', '-gu', '@keyboard')
            prefix()
            os.write(master, key)
            drain()
            table('root')
            os.write(master, b'x')
            drain()
            assert tmux('show', '-gqv', '@keyboard') == ''
        prefix()
        os.write(master, b'\033')
        drain()
        time.sleep(.6)
        table('root')
        print('PASS session Sidebar click, pointer motion, bound/unbound keyboard keys, reload and custom mouse actions')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        # Closing the PTY releases a client blocked flushing its final redraw.
        os.close(master)
        os.close(slave)
        if process:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
