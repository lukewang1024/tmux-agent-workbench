#!/usr/bin/env python3
"""Exercise scoped switch lists through real small tmux clients and touch input."""
from contextlib import ExitStack
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

from daemon_fixture import running_daemon

core = str(Path(sys.argv[1]).resolve())
fixture = str(Path(core).parent / 'examples/codex')
with tempfile.TemporaryDirectory(prefix='wb-switcher-') as directory, ExitStack() as resources:
    root = Path(directory)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               TMUX_AGENT_WORKBENCH_LANGUAGE='en', TMUX_AGENT_WORKBENCH_BIN=core,
               WORKBENCH_FIXTURE_SECONDS='600')
    for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_POPUP', 'WORKBENCH_SOURCE_PANE'):
        env.pop(key, None)
    for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = str(root / key)
    master, slave = pty.openpty()
    other_master, other_slave = pty.openpty()
    clients = []
    popups = []
    size = [40, 20]

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).rstrip('\n')

    def drain(seconds=.1):
        end = time.monotonic() + seconds
        result = b''
        while time.monotonic() < end:
            if select.select([master], [], [], max(0, end - time.monotonic()))[0]:
                result += os.read(master, 65536)
            if select.select([other_master], [], [], 0)[0]:
                os.read(other_master, 65536)
        return result

    def wait(predicate, label):
        end = time.monotonic() + 8
        while not predicate():
            if time.monotonic() >= end:
                raise AssertionError(label)
            drain(.05)

    def text(needle):
        expected = needle.encode()
        output = b''
        end = time.monotonic() + 8
        while expected not in re.sub(rb'\x1b(?:\[[0-?]*[ -/]*[@-~]|[()][A-Z0-9]|[ -/]*[@-~])', b'', output):
            output += drain(.05)
            if time.monotonic() >= end:
                raise AssertionError(('missing switcher content', needle, output))
        return output

    def client_value(name, fmt):
        return dict(line.split('\t', 1) for line in tmux('list-clients', '-F', '#{client_name}\t' + fmt).splitlines())[name]

    def resize(width, height):
        size[:] = [width, height]
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', height, width, 0, 0))
        os.kill(clients[0].pid, signal.SIGWINCH)
        wait(lambda: client_value(client, '#{client_width} #{client_height}') == f'{width} {height}', 'client resize')
        drain(.15)

    def press(x, y):
        os.write(master, f'\x1b[<0;{x};{y}M'.encode())

    def release(x, y):
        os.write(master, f'\x1b[<0;{x};{y}m'.encode())

    def tap(x, y):
        press(x, y)
        release(x, y)

    def open_list(kind):
        drain()
        popup = subprocess.Popen([core, 'switch', kind, '--pane', source, '--client', client], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(popup)
        text('Switch ' + {'window': 'windows', 'session': 'sessions', 'agent': 'agents', 'pane': 'panes'}[kind])
        drain()
        return popup

    def close(popup):
        os.write(master, b'\x1b')
        popup.wait(timeout=5)
        assert popup.returncode == 0, popup.stderr.read()
        drain(.6)

    def reset():
        tmux('switch-client', '-c', client, '-t', 'source:0')
        tmux('select-pane', '-t', source)
        drain(.15)

    def focused(window=None, pane=None, session='source'):
        value = client_value(client, '#{session_name}\t#{window_id}\t#{pane_id}').split('\t')
        return value[0] == session and (window is None or value[1] == window) and (pane is None or value[2] == pane)

    try:
        for fd in (slave, other_slave):
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 20, 40, 0, 0))
        source = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'source', '-n', 'window-zero',
                      '-x', '40', '-y', '20', '-P', '-F', '#{pane_id}', '/bin/sh')
        for key, value in (('mouse', 'on'), ('status', 'off'), ('default-shell', '/bin/sh'),
                           ('status-keys', 'emacs'), ('exit-empty', 'off'), ('exit-unattached', 'off')):
            tmux('set-option', '-g', key, value)
        tmux('set-option', '-sg', 'escape-time', '50')
        tmux('select-pane', '-t', source, '-T', 'pane-left')
        second = tmux('split-window', '-d', '-t', source, '-P', '-F', '#{pane_id}', '/bin/sh')
        tmux('select-pane', '-t', second, '-T', 'pane-right')
        hidden = tmux('split-window', '-d', '-t', source, '-P', '-F', '#{pane_id}', '/bin/sh')
        tmux('set-option', '-p', '-t', hidden, '@pane_role', 'sidebar')
        tmux('select-pane', '-t', hidden, '-T', 'hidden-sidebar')
        windows = [tmux('display-message', '-p', '-t', source, '#{window_id}')]
        agent_pane = None
        for index in range(1, 9):
            name = '中文任务：检查窗口和触屏交互' if index == 1 else f'window-{index}'
            window = tmux('new-window', '-d', '-t', 'source', '-n', name, '-P', '-F', '#{window_id}',
                          shlex.quote(fixture) if index == 1 else '/bin/sh')
            windows.append(window)
            if index == 1:
                agent_pane = tmux('display-message', '-p', '-t', window, '#{pane_id}')
        other = tmux('new-session', '-d', '-s', 'other', '-n', 'other-agent-window', '-P', '-F', '#{pane_id}', shlex.quote(fixture))
        resources.enter_context(running_daemon(core, env))
        for fd in (slave, other_slave):
            clients.append(subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'source'],
                                            env=env, stdin=fd, stdout=fd, stderr=fd))
        wait(lambda: len(tmux('list-clients', '-F', '#{client_name}').splitlines()) == 2, 'two clients attach')
        client = os.ttyname(slave)
        sibling = os.ttyname(other_slave)
        reset()
        # Every line of a three-row card must activate the same window, and
        # press-and-hold must not activate before release.
        for y in (7, 8, 9):
            popup = open_list('window')
            press(10, y)
            drain(.35)
            assert focused(window=windows[0]), 'press activated before release'
            release(10, y)
            popup.wait(timeout=5)
            assert popup.returncode == 0, popup.stderr.read()
            wait(lambda: focused(window=windows[1]), 'card line did not select current-session window')
            reset()
        print('PASS all three card rows select on release, preserving window scope', flush=True)
        popup = open_list('window')
        tap(25, 19)
        text('window-8')
        tap(15, 20)
        text('window-zero')
        os.write(master, b'/window-8')
        text('window-8')
        os.write(master, b'\r')
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[8]), 'search result did not select last window')
        reset()
        print('PASS touch paging and search reach windows absent from the status bar', flush=True)
        popup = open_list('window')
        press(10, 9)
        os.write(master, b'\x1b[<32;10;6M')
        release(10, 6)
        drain(.2)
        assert focused(window=windows[0]) and popup.poll() is None, 'swipe also activated a card'
        close(popup)
        popup = open_list('pane')
        output = drain(.2)
        os.write(master, b'/hidden-sidebar')
        text('No matching results')
        os.write(master, b'\x15')
        text('pane-right')
        tap(10, 9)
        popup.wait(timeout=5)
        wait(lambda: focused(pane=second), 'pane card did not focus the pane')
        reset()
        popup = open_list('session')
        tap(10, 6)
        popup.wait(timeout=5)
        wait(lambda: focused(pane=other, session='other'), 'session card did not restore its active target')
        assert client_value(sibling, '#{session_name}') == 'source'
        reset()
        popup = open_list('agent')
        tap(10, 6)
        popup.wait(timeout=5)
        wait(lambda: focused(pane=other, session='other'), 'Agent list did not focus the actual Agent')
        assert client_value(sibling, '#{session_name}') == 'source'
        reset()
        print('PASS independent session, Agent and pane views, Sidebar exclusion and client isolation', flush=True)
        popup = open_list('window')
        press(10, 7)
        tmux('kill-window', '-t', windows[1])
        drain(1.5)
        release(10, 7)
        drain(.2)
        assert focused(window=windows[0]) and popup.poll() is None, 'refresh retargeted a held touch'
        close(popup)
        for width, height in ((32, 12), (24, 8)):
            resize(width, height)
            popup = open_list('window')
            tap(width * 5 // 8, height)
            drain(.2)
            tap(10, 6)
            popup.wait(timeout=5)
            target = windows[3] if height == 12 else windows[2]
            wait(lambda: focused(window=target), 'small viewport card was not usable')
            reset()
        resize(40, 20)
        popup = open_list('window')
        os.write(master, b'/window-8')
        text('window-8')
        resize(24, 8)
        tap(10, 6)
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[8]), 'keyboard-sized resize lost the selected search result')
        reset()
        resize(40, 20)
        print('PASS disappearing targets and 32/24-column viewports never misroute a touch', flush=True)
        popup = open_list('window')
        tap(5, 19)
        popup.wait(timeout=5)
        text('Switch window')
        os.write(master, b'w')
        text('Switch windows')
        tap(35, 20)
        drain(.6)
        assert focused(window=windows[0])
        assert tmux('display-message', '-p', '-t', source, '#{pane_in_mode}') == '0'
        print('PASS root menu opens the touch list, Back restores the root, and Close leaves native tree mode unused', flush=True)
        # Sidebar keyboard and contextual-menu entries use the same lists,
        # while Back returns to Sidebar rather than an unrelated tmux menu.
        sidebar = subprocess.Popen([core, 'sidebar', '--popup', '--pane', source, '--client', client],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(sidebar)
        text('+ new')
        drain(.2)
        os.write(master, b's')
        text('Switch sessions')
        tap(5, 19)
        text('+ new')
        drain(.2)
        tap(37, 1)
        text('workbench')
        os.write(master, b'a')
        text('Switch agents')
        tap(5, 19)
        text('+ new')
        os.write(master, b'\x1b')
        drain(.7)
        assert focused(window=windows[0])
        print('PASS Sidebar and contextual-menu routes share the touch UI and preserve their Back destination', flush=True)
        # A shared pane ID must still list and focus windows in this client's
        # Session rather than whichever Session tmux finds first.
        tmux('new-session', '-d', '-s', 'linked', '-n', 'linked-only', '/bin/sh')
        tmux('link-window', '-s', 'source:0', '-t', 'linked:5')
        tmux('switch-client', '-c', client, '-t', 'linked:0')
        popup = open_list('window')
        os.write(master, b'/window-8')
        text('No matching results')
        os.write(master, b'\x15')
        text('linked-only')
        tap(10, 9)
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[0], session='linked'), 'linked window chose another Session')
        assert client_value(sibling, '#{session_name}') == 'source'
        reset()
        tmux('kill-session', '-t', 'linked')
        print('PASS linked windows preserve the invoking Session and client', flush=True)
        tmux('respawn-pane', '-k', '-t', other, '/bin/sh')
        popup = open_list('agent')
        text('No running agents')
        tap(10, 6)
        assert popup.poll() is None and focused(window=windows[0])
        close(popup)
        print('PASS an empty Agent view stays usable and never turns a blank row into a target', flush=True)
    finally:
        resources.close()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for process in popups + clients:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        for fd in (master, slave, other_master, other_slave):
            os.close(fd)
