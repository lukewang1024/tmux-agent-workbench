#!/usr/bin/env python3
"""Exercise the pinned focus cards through a real tmux client."""
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
language = sys.argv[2] if len(sys.argv) > 2 else 'zh'
agent_label, sort_label, pinned_menu, unpin_label = (
    ('智能体', '分组', '置顶的智能体', '取消置顶') if language == 'zh' else
    ('Agents', 'grouped', 'Pinned agents', 'Unpin task'))

with tempfile.TemporaryDirectory(prefix='wb-sidebar-focus-') as root, ExitStack() as processes:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               TMUX_AGENT_WORKBENCH_LANGUAGE=language, WORKBENCH_FIXTURE_SECONDS='120')
    for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = root + '/' + key
    automation_bin = Path(root) / 'automation-bin'
    automation_bin.mkdir()
    automation_marker = Path(root) / 'desktop-automation-calls'
    for name in ('osascript', 'open'):
        fake = automation_bin / name
        fake.write_text('#!/bin/sh\nprintf "%s\\n" "$0" >> "$WORKBENCH_TEST_AUTOMATION_MARKER"\nexit 1\n')
        fake.chmod(0o755)
    env['WORKBENCH_TEST_AUTOMATION_MARKER'] = str(automation_marker)
    env['PATH'] = str(automation_bin) + os.pathsep + env.get('PATH', '')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 160, 0, 0))
    client = None

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).rstrip('\n')

    def drain(seconds=.05):
        output = b''
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([master], [], [], max(0, end - time.monotonic()))[0]:
                output += os.read(master, 65536)
        return output

    def screen():
        return tmux('capture-pane', '-p', '-t', sidebar).splitlines()

    def wait(predicate, label):
        deadline = time.monotonic() + 12
        while not predicate():
            if time.monotonic() >= deadline:
                markers = tmux('list-windows', '-a', '-F', '#{session_name}:#{@agent_focus}')
                log = Path(env['XDG_STATE_HOME']) / 'tmux-agent-workbench/popup-actions.log'
                debug = log.with_name('sidebar.log')
                raise AssertionError((label, screen(), markers, log.read_text() if log.exists() else '',
                                      debug.read_text() if debug.exists() else ''))
            drain()

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'a-target', '-n', 'Priority task',
             '-x', '160', '-y', '24', shlex.quote(fixture))
        for option in ('mouse', 'focus-events'):
            tmux('set-option', '-g', option, 'on')
        tmux('set-option', '-g', 'status', 'off')
        for name in ('b-agent', 'c-agent', 'd-agent', 'e-agent', 'f-agent', 'g-agent', 'z-source'):
            tmux('new-session', '-d', '-s', name, '-n', f'Task {name}', '-x', '160', '-y', '24',
                 shlex.quote(fixture))
        tmux('new-session', '-d', '-s', 'm-shell', '-n', 'plain-shell', '-x', '160', '-y', '24',
             'exec sleep 120')
        target = tmux('display-message', '-p', '-t', 'a-target', '#{pane_id}')
        target_window = tmux('display-message', '-p', '-t', target, '#{window_id}')
        shell_window = tmux('display-message', '-p', '-t', 'm-shell', '#{window_id}')
        shell_pane = tmux('display-message', '-p', '-t', 'm-shell', '#{pane_id}')
        shell_sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', shell_window,
                             '-P', '-F', '#{pane_id}', 'exec sleep 120')
        tmux('set-option', '-p', '-t', shell_sidebar, '@pane_role', 'sidebar')
        tmux('select-pane', '-t', shell_sidebar)
        main = tmux('display-message', '-p', '-t', 'z-source', '#{pane_id}')
        for window in (target_window, shell_window):
            tmux('set-option', '-w', '-t', window, '@agent_focus', '1')
        processes.enter_context(running_daemon(core, env))
        sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                       '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        tmux('select-pane', '-t', main)
        client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'z-source'],
                                  env=env, stdin=slave, stdout=slave, stderr=slave,
                                  start_new_session=True)
        wait(lambda: len(screen()) > 6 and agent_label in screen()[0] and ' 2' in screen()[0]
             and sort_label in screen()[0] and '' in screen()[0]
             and '' in screen()[0] and '' in screen()[0]
             and 'Priority task' in screen()[1] and 'plain-shell' in screen()[3], 'powerline header')
        assert 'a-target' in screen()[2] and 'm-shell' in screen()[4], screen()
        assert ('Agents' not in screen()[0] if language == 'zh' else '智能体' not in screen()[0]), screen()
        assert not any('项已隐藏' in line or ' hidden' in line for line in screen()), screen()
        wait(lambda: any(line.strip().endswith('Priority task') for line in screen()[6:]),
             'ordinary agent card')
        before = tmux('list-panes', '-a', '-F', '#{session_name}:#{window_id}:#{pane_pid}')

        # The count is a touch target for all pinned tasks, including those
        # that would not fit on a short screen.
        before_pin = screen()[0].split('', 1)[0]
        pin_column = sum(2 if unicodedata.east_asian_width(ch) in ('W', 'F') else 1
                         for ch in before_pin) + 1
        os.write(master, f'\x1b[<0;{pin_column};1M\x1b[<0;{pin_column};1m'.encode())
        output = b''
        deadline = time.monotonic() + 8
        while pinned_menu.encode() not in output and time.monotonic() < deadline:
            output += drain()
        assert pinned_menu.encode() in output and b'Priority task' in output and b'plain-shell' in output, output[-1500:]
        os.write(master, b'2')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'm-shell:{shell_pane}',
             'focus shell from pinned menu')
        client_name = tmux('list-clients', '-F', '#{client_name}').splitlines()[0]
        tmux('switch-client', '-c', client_name, '-t', 'z-source')
        tmux('select-pane', '-t', main)
        drain(.3)

        # A shell-only card remains usable even when its sidebar was last active.
        tmux('select-pane', '-t', sidebar)
        os.write(master, b'2')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'm-shell:{shell_pane}',
             'focus shell card')
        tmux('switch-client', '-c', client_name, '-t', 'z-source')
        tmux('select-pane', '-t', main)

        # Keyboard navigation crosses from pinned cards to ordinary rows in
        # both directions, and Enter opens the selected pinned card.
        tmux('select-pane', '-t', sidebar)
        tmux('send-keys', '-t', sidebar, 'Home', 'Down', 'Down', 'Up', 'Enter')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'm-shell:{shell_pane}',
             'keyboard navigation did not return from ordinary rows to pinned cards')
        assert not automation_marker.exists(), 'local sidebar focus invoked desktop terminal automation'
        tmux('switch-client', '-c', client_name, '-t', 'z-source')
        tmux('select-pane', '-t', sidebar)
        tmux('send-keys', '-t', sidebar, 'Home')
        drain(.2)
        os.write(master, b'\x1b[<35;5;2M\x1b[<65;5;2M')
        drain(.2)
        tmux('send-keys', '-t', sidebar, 'Enter')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'm-shell:{shell_pane}',
             'mouse wheel did not move between pinned cards')
        tmux('switch-client', '-c', client_name, '-t', 'z-source')
        tmux('select-pane', '-t', sidebar)
        tmux('send-keys', '-t', sidebar, 'Home')
        drain(.2)
        # A touch drag keeps its press cancelled and can cross from a pinned
        # card into ordinary rows without activating anything on release.
        os.write(master, b'\x1b[<0;5;2M\x1b[<32;5;1M\x1b[<0;5;1m')
        drain(.2)
        tmux('send-keys', '-t', sidebar, 'Down', 'Down', 'Enter')
        b_pane = tmux('display-message', '-p', '-t', 'b-agent', '#{pane_id}')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'b-agent:{b_pane}',
             'touch drag did not cross from pinned cards into ordinary rows')
        tmux('switch-client', '-c', client_name, '-t', 'z-source')
        tmux('select-pane', '-t', main)

        # Normal scrolling never dislodges the focus cards.
        tmux('send-keys', '-t', sidebar, '-N', '12', 'Down')
        wait(lambda: screen()[6].strip() != 'a-target', 'ordinary list did not scroll')
        assert 'Priority task' in screen()[1] and 'plain-shell' in screen()[3], screen()
        tmux('send-keys', '-t', sidebar, 'Home')
        wait(lambda: screen()[6].strip() == 'a-target', 'Home did not restore the top')

        # The selected ordinary agent can be toggled without visiting its pane.
        tmux('select-pane', '-t', sidebar)
        drain(.2)
        # A prior mouse hover on the shell card must not override later
        # keyboard navigation to a different task.
        os.write(master, b'\x1b[<35;5;4M')
        drain(.2)
        tmux('send-keys', '-t', sidebar, 'Home')
        tmux('send-keys', '-t', sidebar, '-N', '3', 'Down')
        drain(.3)
        b_window = tmux('display-message', '-p', '-t', 'b-agent', '#{window_id}')
        os.write(master, b'p')
        wait(lambda: tmux('show-option', '-wqv', '-t', b_window, '@agent_focus') == '1',
             'keyboard selection after focus hover')
        assert tmux('show-option', '-wqv', '-t', shell_window, '@agent_focus') == '1'
        os.write(master, b'p')
        wait(lambda: tmux('show-option', '-wqv', '-t', b_window, '@agent_focus') == '', 'unfocus b')
        tmux('send-keys', '-t', sidebar, 'Home')
        wait(lambda: screen()[6].strip() == 'a-target', 'ordinary list did not return to top')
        os.write(master, b'p')
        wait(lambda: tmux('show-option', '-wqv', '-t', target_window, '@agent_focus') == '', 'remove with p')
        wait(lambda: 'plain-shell' in screen()[1], 'updated focus cards')
        tmux('send-keys', '-t', sidebar, 'Home', 'Down')
        os.write(master, b'p')
        wait(lambda: tmux('show-option', '-wqv', '-t', target_window, '@agent_focus') == '1', 'add with p')
        wait(lambda: 'Priority task' in screen()[1], 'focus card returns')

        # The pinned card exposes removal through its context menu as well.
        os.write(master, b'\x1b[<2;5;2M\x1b[<2;5;2m')
        menu_output = b''
        deadline = time.monotonic() + 8
        while unpin_label.encode() not in menu_output and time.monotonic() < deadline:
            menu_output += drain()
        assert unpin_label.encode() in menu_output, menu_output[-1000:]
        os.write(master, b'p')
        wait(lambda: tmux('show-option', '-wqv', '-t', target_window, '@agent_focus') == '', 'context menu removal')
        wait(lambda: 'plain-shell' in screen()[1], 'context removal reflected in the sidebar')
        tmux('set-option', '-w', '-t', target_window, '@agent_focus', '1')
        wait(lambda: 'Priority task' in screen()[1], 'focus card returns after external change')

        # A mouse click on either line focuses the exact original agent pane.
        tmux('select-pane', '-t', main)
        drain(.3)
        os.write(master, b'\x1b[<0;5;2M\x1b[<0;5;2m')
        wait(lambda: tmux('list-clients', '-F', '#{session_name}:#{pane_id}') == f'a-target:{target}',
             'focus card click')
        assert before == tmux('list-panes', '-a', '-F', '#{session_name}:#{window_id}:#{pane_pid}')
        print('PASS focus cards stay pinned, toggle in the sidebar, and focus the original task', flush=True)
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
