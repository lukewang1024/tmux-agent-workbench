#!/usr/bin/env python
"""Mouse selection uses the visible item and remains separate from hover and focus."""
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

from daemon_fixture import running_daemon

core = str(Path(sys.argv[1]).resolve())
fixture = str(Path(__file__).resolve().parents[1] / 'target/debug/examples/codex')


def check(height, target, section='agent'):
    with tempfile.TemporaryDirectory(prefix='wb-sidebar-mouse-') as root, ExitStack() as processes:
        socket = root + '/tmux.sock'
        env = dict(os.environ, TERM='xterm-256color',
                   TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
                   WORKBENCH_FIXTURE_SECONDS='120')
        for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP', 'NO_COLOR'):
            env.pop(key, None)
        for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
            env[key] = root + '/' + key
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', height, 160, 0, 0))
        client = None

        def tmux(*args):
            return subprocess.check_output(['tmux', '-S', socket, *args],
                                           env=env, text=True).rstrip('\n')

        def drain(seconds):
            end = time.monotonic() + seconds
            output = b''
            while time.monotonic() < end:
                ready, _, _ = select.select([master], [], [], max(0, end - time.monotonic()))
                if ready:
                    output += os.read(master, 65536)
            return output

        def wait_for(predicate, label):
            deadline = time.monotonic() + 10
            while not predicate():
                if time.monotonic() >= deadline:
                    raise AssertionError((label, screen(), tmux("list-panes", "-a", "-F", "#{session_name} #{window_name}")))
                drain(0.05)

        def screen():
            return tmux('capture-pane', '-p', '-t', sidebar).splitlines()

        try:
            tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'a-target',
                 '-n', 'Task a-target', '-x', '160', '-y', str(height), shlex.quote(fixture))
            for option in ('mouse', 'focus-events'):
                tmux('set-option', '-g', option, 'on')
            tmux('set-option', '-g', 'status', 'off')
            for name in ('b-agent', 'c-agent', 'd-agent', 'e-agent', 'z-source'):
                tmux('new-session', '-d', '-s', name, '-n', f'Task {name}', '-x', '160', '-y', str(height),
                     shlex.quote(fixture))
            main = tmux('display-message', '-p', '-t', 'z-source', '#{pane_id}')
            target_pane = tmux('display-message', '-p', '-t', target, '#{pane_id}')
            support = None
            if height == 40:
                tmux('set-option', '-g', '@adaptive_palette',
                     '{"version":1,"accent":"colour72","warning":"colour166"}')
                support = tmux('split-window', '-d', '-h', '-t', main,
                               '-P', '-F', '#{pane_id}', 'exec sleep 120')
                tmux('set-option', '-w', '-t', main, '@agent_focus', '1')
            processes.enter_context(running_daemon(core, env))
            sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                           '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            tmux('select-pane', '-t', main)
            client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'z-source'],
                                      env=env, stdin=slave, stdout=slave, stderr=slave,
                                      start_new_session=True)
            wait_for(lambda: any('Task ' in line for line in screen()), 'sidebar did not render')
            def source_cards_marked(expected):
                lines = screen()
                titles = [index for index, line in enumerate(lines) if 'Task z-source' in line]
                return len(titles) == 2 and all(
                    index + 1 < len(lines)
                    and lines[index].startswith('▏') == expected
                    and lines[index + 1].startswith('▏') == expected
                    for index in titles
                )

            def marker_color_matches(color):
                lines = tmux('capture-pane', '-p', '-e', '-t', sidebar).splitlines()
                markers = [line.partition('▏')[0] for line in lines if '▏' in line]
                return len(markers) == 4 and all(f'38;5;{color}m' in prefix for prefix in markers)

            if support is not None:
                wait_for(lambda: source_cards_marked(True), 'both lines of the foreground cards must be marked')
                wait_for(lambda: marker_color_matches(72), 'foreground markers did not use the idle color')
                tmux('select-pane', '-t', main, '-T', '⠴ Task z-source')
                wait_for(lambda: marker_color_matches(166), 'foreground markers did not follow the working state')
                tmux('select-pane', '-t', main, '-T', 'Ready')
                wait_for(lambda: marker_color_matches(72), 'foreground markers did not return to the idle color')
                tmux('select-pane', '-t', support)
                wait_for(lambda: source_cards_marked(False), 'foreground marker remained while the shell was active')
                tmux('select-pane', '-t', main)
                wait_for(lambda: source_cards_marked(True), 'foreground marker did not return to the Agent')
            # Let the initial snapshot select the current agent, then browse back
            # to the top without giving the sidebar focus, as mouse scrolling does.
            drain(1.3)
            if section == 'session':
                tmux('send-keys', '-t', sidebar, 'Tab')
                wait_for(lambda: 'Sessions' in screen()[0], 'session view did not open')
            tmux('send-keys', '-t', sidebar, '-N', '40', 'Up')
            wait_for(lambda: any('a-target' in line for line in screen()), 'scroll did not finish')
            # Group headings add rows between sessions. Browse far enough to
            # see the actual agent card, without focusing the sidebar pane.
            def target_row(line):
                title = f'Task {target}' if section == 'agent' else target
                content = line.strip().removesuffix('').rstrip() if section == 'session' else line.strip()
                return content in (f'○ {title}', f'● {title}', f'! {title}', f'? {title}')

            for _ in range(20):
                if any(target_row(line) for line in screen()):
                    break
                tmux('send-keys', '-t', sidebar, 'Down')
                drain(.1)
            wait_for(lambda: any(target_row(line) for line in screen()), 'target card not visible')
            drain(0.15)
            rows = screen()
            assert ('Sessions' if section == 'session' else 'Agents') in rows[0] and '' in rows[0], rows
            row = next(i for i, line in enumerate(rows) if target_row(line))
            assert tmux('list-clients', '-F', '#{pane_id}') == main
            # Keep this menu tall enough for all actions while the sidebar
            # itself overflows; the 12-line cases below cover first-click
            # navigation on short clients.
            if height == 16 and target == 'e-agent':
                # The menu icon is the rightmost cell; the preceding cell is
                # now the full-height separator between sort and menu.
                os.write(master, b'\x1b[<0;36;1M\x1b[<0;36;1m')
                output = b''
                deadline = time.monotonic() + 5
                expected = (b'Show / hide Sidebar', b'Toggle details',
                            b'Next attention', b'Reload Workbench')
                while not all(label in output for label in expected) and time.monotonic() < deadline:
                    output += drain(.05)
                assert all(label in output for label in expected), output
                os.write(master, b'\x1b')
                drain(.7)
                tmux('select-pane', '-t', main)
                drain(.1)
                assert 'Agents' in screen()[0] and '' in screen()[0]
                print('PASS: scrolled sidebar opens its pinned top menu')
                # All six fixture Agents are idle. Clicking the green count
                # hides them; clicking it again restores the visible rows.
                state_counts = '0  0  0  6'
                header = screen()[0]
                assert state_counts in header, header
                idle_x = header.index(state_counts) + len(state_counts)
                fixture_agents = ('a-target', 'b-agent', 'c-agent', 'd-agent', 'e-agent', 'z-source')

                def fixture_cards_visible():
                    return any(
                        f'Task {name}' in line
                        for line in screen()
                        for name in fixture_agents
                    )

                os.write(master, f'\x1b[<0;{idle_x};1M\x1b[<0;{idle_x};1m'.encode())
                wait_for(lambda: not fixture_cards_visible(), 'idle count did not filter Agent rows')
                os.write(master, f'\x1b[<0;{idle_x};1M\x1b[<0;{idle_x};1m'.encode())
                wait_for(lambda: fixture_cards_visible(), 'idle count did not restore Agent rows')
                rows = screen()
                row = next(i for i, line in enumerate(rows) if target_row(line))
                print('PASS: idle count toggles Agent visibility and restores the rows')
            # Give up the keyboard selection without resetting the viewport.
            # tmux emits FocusGained before forwarding the next MouseDown.
            tmux('select-pane', '-t', sidebar)
            drain(.15)
            tmux('select-pane', '-t', main)
            drain(.2)
            os.write(master, f'\x1b[<35;5;{row + 1}M'.encode())
            drain(0.1)
            click = f'\x1b[<0;5;{row + 1}M\x1b[<0;5;{row + 1}m'.encode()
            os.write(master, click)
            wait_for(lambda: tmux('list-clients', '-F', '#{pane_id}') == sidebar,
                     f'first {section} click did not focus the sidebar')
            drain(.2)
            assert tmux('list-clients', '-F', '#{pane_id}') == sidebar
            if height == 40:
                drain(1.3)
                assert source_cards_marked(True), screen()
                assert not any(line.startswith('▏') and target in line for line in screen())
                print('PASS: foreground markers follow Agent status and pane focus and survive sidebar selection')
                # Hover over another Agent and use the real keyboard binding.
                # Both p and Enter must still target the clicked Agent.
                other_row = next(i for i, line in enumerate(screen()) if 'Task b-agent' in line)
                os.write(master, f'\x1b[<35;5;{other_row + 1}M'.encode())
                drain(.15)
                other_window = tmux('display-message', '-p', '-t', 'b-agent', '#{window_id}')
                target_window = tmux('display-message', '-p', '-t', target, '#{window_id}')
                os.write(master, b'p')
                wait_for(lambda: tmux('show-option', '-wqv', '-t', target_window, '@agent_focus') == '1',
                         'p did not pin the clicked Agent')
                assert tmux('show-option', '-wqv', '-t', other_window, '@agent_focus') == ''
                assert tmux('list-clients', '-F', '#{pane_id}') == sidebar
                os.write(master, b'\r')
            else:
                os.write(master, click)
            try:
                wait_for(lambda: tmux('list-clients', '-F', '#{pane_id}') == target_pane,
                         f'activation did not focus {target}')
            except AssertionError:
                raise AssertionError((height, target, rows,
                                      tmux('list-clients', '-F', '#{session_name} #{pane_id}'),
                                      screen())) from None
            print(f'PASS: select and activate {section} {target} at height {height}')
        finally:
            # A detached client can block flushing its final redraw on macOS.
            # Release the PTY before waiting for the client to be reaped.
            os.close(master)
            os.close(slave)
            if client is not None:
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait(timeout=5)
            subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


check(12, 'e-agent')
check(12, 'a-target')
check(16, 'e-agent')
check(40, 'a-target')
check(60, 'a-target', 'session')
