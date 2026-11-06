#!/usr/bin/env python3
"""Exercise independent Agent and Session pins through an attached tmux client."""
from contextlib import ExitStack
import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

from daemon_fixture import running_daemon

core = str(Path(sys.argv[1]).resolve())
fixture = str(Path(__file__).resolve().parents[1] / 'target/debug/examples/codex')
language = sys.argv[2] if len(sys.argv) > 2 else 'en'
agents, sessions, pinned_sessions, unpin = (
    ('智能体', '会话', '置顶的会话', '取消置顶会话') if language == 'zh' else
    ('Agents', 'Sessions', 'Pinned sessions', 'Unpin session'))

with tempfile.TemporaryDirectory(prefix='wb-sidebar-views-') as root, ExitStack() as processes:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               TMUX_AGENT_WORKBENCH_LANGUAGE=language, WORKBENCH_FIXTURE_SECONDS='120')
    for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR', 'XDG_DATA_HOME'):
        env[key] = root + '/' + key
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 160, 0, 0))
    client = None
    sidebar = None

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).rstrip('\n')

    def drain(seconds=.05):
        data = b''
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([master], [], [], max(0, until - time.monotonic()))[0]:
                data += os.read(master, 65536)
        return data

    def screen():
        lines = tmux('capture-pane', '-p', '-t', sidebar).splitlines()
        return lines if lines else ['']

    def wait(predicate, label):
        deadline = time.monotonic() + 12
        while not predicate():
            if time.monotonic() >= deadline:
                path = Path(env['XDG_STATE_HOME']) / 'tmux-agent-workbench/sidebar.log'
                raise AssertionError((label, screen(), path.read_text() if path.exists() else ''))
            drain()

    def return_to_source():
        tmux('switch-client', '-c', client_name, '-t', 'z-source')
        tmux('select-pane', '-t', sidebar)
        drain(.2)

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'a-target', '-n', 'Priority task',
             '-x', '160', '-y', '24', shlex.quote(fixture))
        for option in ('mouse', 'focus-events'):
            tmux('set-option', '-g', option, 'on')
        tmux('set-option', '-g', 'status', 'off')
        tmux('new-session', '-d', '-s', 'b-shell', '-n', 'other task', 'exec sleep 120')
        tmux('new-session', '-d', '-s', 'z-source', '-n', 'source', 'exec sleep 120')
        a_window = tmux('display-message', '-p', '-t', 'a-target', '#{window_id}')
        b_pane = tmux('display-message', '-p', '-t', 'b-shell', '#{pane_id}')
        b_id = tmux('display-message', '-p', '-t', 'b-shell', '#{session_id}')
        main = tmux('display-message', '-p', '-t', 'z-source', '#{pane_id}')
        tmux('set-option', '-w', '-t', a_window, '@agent_focus', '1')
        processes.enter_context(running_daemon(core, env))
        sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                       '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        tmux('select-pane', '-t', main)
        client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'z-source'], env=env,
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        wait(lambda: agents in screen()[0] and ' 1' in screen()[0]
             and 'Priority task' in screen()[1], 'initial Agent view and its pinned task')
        assert not any('b-shell' in line for line in screen()), screen()
        client_name = tmux('list-clients', '-F', '#{client_name}').splitlines()[0]
        # The first Powerline label is a real touch/click target.
        os.write(master, b'\x1b[<0;2;1M\x1b[<0;2;1m')
        wait(lambda: sessions in screen()[0] and ' 0' in screen()[0]
             and any('b-shell' in line for line in screen()), 'switch to Session view')
        assert not any('Priority task' in line for line in screen()), screen()
        assert ('Sessions' not in screen()[0] if language == 'zh' else '会话' not in screen()[0]), screen()
        # The pin icon on the session row works with an ordinary touch/click.
        session_row = next(i for i, line in enumerate(screen()) if 'b-shell' in line)
        sidebar_width = int(tmux('display-message', '-p', '-t', sidebar, '#{pane_width}'))
        os.write(master, f'\x1b[<0;{sidebar_width - 1};{session_row + 1}M\x1b[<0;{sidebar_width - 1};{session_row + 1}m'.encode())
        assert tmux('list-clients', '-F', '#{session_name}') == 'z-source'
        tmux('select-pane', '-t', sidebar)
        wait(lambda: tmux('show-option', '-qv', '-t', b_id, '@workbench_session_focus') == '1',
             'pin selected session')
        wait(lambda: ' 1' in screen()[0] and 'b-shell' in screen()[1], 'pinned session card')
        assert tmux('show-option', '-wqv', '-t', a_window, '@agent_focus') == '1'
        # Cross from the pinned card into the list and back, then open it.
        tmux('send-keys', '-t', sidebar, 'Home', 'Down', 'Up', 'Enter')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'b-shell:{b_pane}',
             'keyboard crosses from ordinary Session rows to pinned Session')
        return_to_source()
        # Real wheel input also traverses the boundary.
        tmux('send-keys', '-t', sidebar, 'Home', 'Down')
        drain(.2)
        os.write(master, b'\x1b[<35;5;4M\x1b[<64;5;4M')
        drain(.2)
        tmux('send-keys', '-t', sidebar, 'Enter')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'b-shell:{b_pane}',
             'wheel crosses from ordinary Session rows to pinned Session')
        return_to_source()
        # The count menu and card context menu use Session-specific wording.
        row = screen()[0]
        before_pin = row.split('', 1)[0]
        pin_column = sum(2 if unicodedata.east_asian_width(ch) in ('W', 'F') else 1
                         for ch in before_pin) + 1
        os.write(master, f'\x1b[<0;{pin_column};1M\x1b[<0;{pin_column};1m'.encode())
        output = b''
        deadline = time.monotonic() + 8
        while pinned_sessions.encode() not in output and time.monotonic() < deadline:
            output += drain()
        assert pinned_sessions.encode() in output and b'b-shell' in output, output[-1500:]
        os.write(master, b'\x1b')
        drain(.7)
        tmux('select-pane', '-t', sidebar)
        drain(.2)
        os.write(master, b'\x1b[<2;5;2M\x1b[<2;5;2m')
        output = b''
        deadline = time.monotonic() + 8
        while unpin.encode() not in output and time.monotonic() < deadline:
            output += drain()
        assert unpin.encode() in output, output[-1500:]
        os.write(master, b'p')
        wait(lambda: tmux('show-option', '-qv', '-t', b_id, '@workbench_session_focus') == '', 'unpin session')
        wait(lambda: ' 0' in screen()[0], 'unpin reflected in Session view')
        # Re-pin, then switch views and verify each retains only its own pins.
        tmux('send-keys', '-t', sidebar, 'Home', 'Down', 'p')
        wait(lambda: tmux('show-option', '-qv', '-t', b_id, '@workbench_session_focus') == '1', 'repin')
        wait(lambda: 'b-shell' in screen()[1], 'repin shown')
        tmux('send-keys', '-t', sidebar, 'Tab')
        wait(lambda: agents in screen()[0] and 'Priority task' in screen()[1], 'return to Agent view')
        assert not any('b-shell' in line for line in screen()), screen()
        tmux('send-keys', '-t', sidebar, 'Tab')
        wait(lambda: sessions in screen()[0] and 'b-shell' in screen()[1], 'session pin persists across views')
        # A new sidebar process restores the remembered view and session pin.
        tmux('kill-pane', '-t', sidebar)
        sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                       '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        wait(lambda: sessions in screen()[0] and 'b-shell' in screen()[1], 'view and pin survive reopen')
        print(f'PASS {language}: independent Agent and Session views, pinning, navigation, menus and persistence', flush=True)
    finally:
        os.close(master)
        os.close(slave)
        if client is not None:
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)
        processes.close()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
