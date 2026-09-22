#!/usr/bin/env python3
"""First-click navigation must use the row visible before tmux focuses the sidebar."""
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

core = str(Path(sys.argv[1]).resolve())
fixture = str(Path(__file__).resolve().parents[1] / 'target/debug/examples/codex')


def check(height, target):
    with tempfile.TemporaryDirectory(prefix='wb-sidebar-mouse-') as root:
        socket = root + '/tmux.sock'
        env = dict(os.environ, TERM='xterm-256color',
                   TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
                   WORKBENCH_FIXTURE_SECONDS='120')
        for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
            env.pop(key, None)
        for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
            env[key] = root + '/' + key
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', height, 160, 0, 0))
        client = None

        def tmux(*args):
            return subprocess.check_output(['tmux', '-S', socket, *args],
                                           env=env, text=True).strip()

        def drain(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                ready, _, _ = select.select([master], [], [], max(0, end - time.monotonic()))
                if ready:
                    os.read(master, 65536)

        def wait_for(predicate, label):
            deadline = time.monotonic() + 10
            while not predicate():
                if time.monotonic() >= deadline:
                    raise AssertionError(label)
                drain(0.05)

        def screen():
            return tmux('capture-pane', '-p', '-t', sidebar).splitlines()

        try:
            tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'a-target',
                 '-x', '160', '-y', str(height), shlex.quote(fixture))
            for option in ('mouse', 'focus-events'):
                tmux('set-option', '-g', option, 'on')
            tmux('set-option', '-g', 'status', 'off')
            for name in ('b-agent', 'c-agent', 'd-agent', 'e-agent', 'z-source'):
                tmux('new-session', '-d', '-s', name, '-x', '160', '-y', str(height),
                     shlex.quote(fixture))
            main = tmux('display-message', '-p', '-t', 'z-source', '#{pane_id}')
            target_pane = tmux('display-message', '-p', '-t', target, '#{pane_id}')
            subprocess.check_call([core, 'daemon', 'ensure'], env=env, stdout=subprocess.DEVNULL)
            sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                           '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            tmux('select-pane', '-t', main)
            client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'z-source'],
                                      env=env, stdin=slave, stdout=slave, stderr=slave,
                                      start_new_session=True)
            wait_for(lambda: any(target in line for line in screen()), 'sidebar did not render')
            # Let the initial snapshot select the current agent, then browse back
            # to the top without giving the sidebar focus, as mouse scrolling does.
            drain(1.3)
            tmux('send-keys', '-t', sidebar, '-N', '40', 'Up')
            wait_for(lambda: any('a-target' in line for line in screen()), 'scroll did not finish')
            drain(0.15)
            rows = screen()
            row = max(i for i, line in enumerate(rows) if target in line)
            assert tmux('list-clients', '-F', '#{pane_id}') == main
            # Real terminal mouse input: tmux emits FocusGained before forwarding
            # MouseDown. The short viewport used to scroll here and hit z-source
            # (or b-agent), while the tall viewport already worked.
            os.write(master, f'\x1b[<35;5;{row + 1}M'.encode())
            drain(0.1)
            os.write(master, f'\x1b[<0;5;{row + 1}M\x1b[<0;5;{row + 1}m'.encode())
            try:
                wait_for(lambda: tmux('list-clients', '-F', '#{pane_id}') == target_pane,
                         f'first click did not focus {target}')
            except AssertionError:
                raise AssertionError((height, target, rows,
                                      tmux('list-clients', '-F', '#{session_name} #{pane_id}'),
                                      screen())) from None
            print(f'PASS: first click focuses {target} at height {height}')
        finally:
            subprocess.run([core, 'daemon', 'stop'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if client is not None:
                client.terminate()
                client.wait(timeout=5)
            subprocess.run(['tmux', '-S', socket, 'kill-server'],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            os.close(master)
            os.close(slave)


check(12, 'e-agent')
check(12, 'a-target')
check(40, 'a-target')
