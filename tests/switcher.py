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
import unicodedata

from daemon_fixture import running_daemon

repo = Path(__file__).resolve().parents[1]
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
    # Back and Sidebar actions use the installed shell helpers. Supply this
    # checkout's helpers so the fixture also works on an uninstalled machine.
    shim = root / 'bin'
    shim.mkdir()
    (shim / 'tmux-agent-workbench').symlink_to(repo / 'bin/tmux-agent-workbench-cli')
    env['PATH'] = str(shim) + ':' + str(repo / 'bin') + ':' + env['PATH']
    master, slave = pty.openpty()
    other_master, other_slave = pty.openpty()
    clients = []
    popups = []
    size = [40, 20]
    popup_size = [0, 0, False]
    painted = bytearray()

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
        painted.extend(result)
        return result

    def wait(predicate, label):
        end = time.monotonic() + 8
        while not predicate():
            if time.monotonic() >= end:
                raise AssertionError((label, screen_rows(),
                                      tmux('list-windows', '-a', '-F', '#{session_name} #{window_id} #{window_name}')))
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

    def prepare_popup():
        painted.clear()
        width, height = size
        border = width >= 36 and height >= 12
        popup_size[:] = [min(96, max(32, width * 9 // 10)) if border else width,
                         min(40, max(12, height * 9 // 10)) if border else height, border]

    def screen_rows():
        # Observe the shared surface's actual card position and border.
        screen = {}
        x = y = 0
        tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b[ -/]*[0-~]|[^\x1b]',
                            painted.decode(errors='replace'))
        for token in tokens:
            if token.startswith('\x1b['):
                command, values = token[-1], token[2:-1]
                if any(char not in '0123456789;' for char in values):
                    continue
                numbers = [int(value or 0) for value in values.split(';')]
                amount = numbers[0] or 1
                if command in ('H', 'f'):
                    y, x = amount - 1, (numbers[1] or 1) - 1 if len(numbers) > 1 else 0
                elif command == 'G': x = amount - 1
                elif command == 'd': y = amount - 1
                elif command == 'C': x += amount
                elif command == 'D': x -= amount
                elif command == 'A': y -= amount
                elif command == 'B': y += amount
                elif command in ('J', 'K', 'X'):
                    mode = numbers[0]
                    for col, row in list(screen):
                        if command == 'J':
                            erase = mode == 2 or (mode == 0 and (row, col) >= (y, x)) or (mode == 1 and (row, col) <= (y, x))
                        elif command == 'K':
                            erase = row == y and (mode == 2 or (mode == 0 and col >= x) or (mode == 1 and col <= x))
                        else:
                            erase = row == y and x <= col < x + amount
                        if erase:
                            del screen[col, row]
            elif token.startswith('\x1b'):
                continue
            elif token == '\r': x = 0
            elif token == '\n': y += 1
            elif token.isprintable() and not unicodedata.combining(token):
                screen[x, y] = token
                x += 2 if unicodedata.east_asian_width(token) in ('W', 'F') else 1
        return [''.join(screen.get((col, row), ' ') for col in range(size[0]))
                for row in range(size[1])]

    def content_rect():
        if size[0] < 36 or size[1] < 12:
            return 0, 0, size[0], size[1]
        rows = screen_rows()
        for top, row in enumerate(rows):
            if '┌' not in row or '┐' not in row:
                continue
            left, right = row.index('┌'), row.index('┐')
            for bottom in range(top + 1, len(rows)):
                if rows[bottom][left] == '└' and rows[bottom][right] == '┘':
                    assert right - left + 1 == min(96, max(32, size[0] * 9 // 10)), rows
                    assert bottom - top + 1 == min(40, max(12, size[1] * 9 // 10)), rows
                    return left + 1, top + 1, right - left - 1, bottom - top - 1
        raise AssertionError(('missing Sidebar popup border', rows))

    def position(x, y):
        left, top, _, _ = content_rect()
        return x + left, y + top

    def press(x, y):
        x, y = position(x, y)
        os.write(master, f'\x1b[<0;{x};{y}M'.encode())

    def release(x, y):
        x, y = position(x, y)
        os.write(master, f'\x1b[<0;{x};{y}m'.encode())

    def tap(x, y):
        press(x, y)
        release(x, y)

    def footer(index):
        _, _, width, height = content_rect()
        if index in (1, 2):
            os.write(master, b'\x1b[5~' if index == 1 else b'\x1b[6~')
        else:
            tap(width // 2, height - (2 if height >= 10 else 1) if index == 0 else height)

    def wheel(down, count=1):
        x, y = position(10, 1)
        os.write(master, (f'\x1b[<{65 if down else 64};{x};{y}M' * count).encode())

    def visible_rows():
        left, top, width, height = content_rect()
        return [row[left:left + width] for row in screen_rows()[top:top + height]]

    def row_text(row):
        # screen_rows keeps a blank continuation cell for wide glyphs so
        # pointer coordinates stay physical; text comparisons skip that cell.
        result, index = '', 0
        while index < len(row):
            character = row[index]
            result += character
            index += 2 if unicodedata.east_asian_width(character) in ('W', 'F') else 1
        return result

    def first_row():
        return 5 if content_rect()[3] >= 10 else 3 if content_rect()[3] >= 6 else 2

    def open_list(kind):
        drain()
        prepare_popup()
        popup = subprocess.Popen([core, 'switch', kind, '--pane', source, '--client', client], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(popup)
        text('Switch ' + {'window': 'windows', 'session': 'sessions', 'agent': 'agents', 'pane': 'panes'}[kind])
        drain()
        return popup

    def search_list(query):
        # Start search as a key, then wait for the rendered query. Matching a
        # raw label can succeed on an old frame before filtering has happened.
        os.write(master, b'/')
        wait(lambda: row_text(visible_rows()[2]).strip() == '/', 'search mode did not open')
        os.write(master, query.encode())
        wait(lambda: row_text(visible_rows()[2]).strip() == '/ ' + query, 'search query did not render')

    def finish_popup(popup):
        try:
            popup.wait(timeout=5)
        except subprocess.TimeoutExpired as error:
            raise AssertionError(('popup did not finish', popup.args, screen_rows(),
                                  tmux('list-windows', '-a', '-F', '#{session_name} #{window_id} #{window_name}'))) from error

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
                           ('status-keys', 'emacs'), ('exit-empty', 'off'), ('exit-unattached', 'off'),
                           ('@workbench-ui', 'new')):
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
        for mode in ('old', 'new'):
            subprocess.check_call([core, 'ui', mode], env=env, stdout=subprocess.DEVNULL)
            for kind, query, target_window, target_pane, target_session in [
                ('window', '中文', windows[1], None, 'source'),
                ('pane', 'pane-right', None, second, 'source'),
                ('session', 'other', None, other, 'other'),
                ('agent', 'other-agent-window', None, other, 'other'),
            ]:
                print('Checking scoped search:', mode, kind, query, flush=True)
                popup = open_list(kind)
                rows = visible_rows()
                assert rows[first_row() - 2].strip('─ ') == '' and '─' in rows[first_row() - 2], rows
                assert rows[-5].strip('─ ') == '' and '─' in rows[-5], rows
                search_list(query)
                wait(lambda: query in row_text(visible_rows()[first_row() - 1]), 'filtered target did not render')
                os.write(master, b'\r')
                finish_popup(popup)
                wait(lambda: focused(window=target_window, pane=target_pane, session=target_session),
                     'shared switcher lost its scope or callback target in ' + mode)
                assert client_value(sibling, '#{session_name}') == 'source'
                reset()
        print('PASS both menu modes use the same divided switch lists, search, identity guards and client isolation', flush=True)
        # All bounded navigation panels must receive backdrop clicks, including
        # overview Sidebar and every filtered list. A hover/scroll/drag outside
        # cancels a held action; only a complete outside tap dismisses.
        tmux('bind-key', '-n', 'MouseDown1Pane', 'set-option', '-g', '@backdrop-leaked', 'yes')
        for kind in ('window', 'session', 'agent', 'pane', 'sidebar'):
            if kind == 'sidebar':
                prepare_popup()
                popup = subprocess.Popen([core, 'sidebar', '--popup', '--pane', source, '--client', client],
                                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                popups.append(popup)
                text('+ new')
                drain(.15)
            else:
                popup = open_list(kind)
            os.write(master, b'\x1b[<35;1;1M\x1b[<65;1;1M')
            drain(.1)
            assert popup.poll() is None, (kind, 'outside hover/wheel dismissed')
            press(10, 5)
            os.write(master, b'\x1b[<32;1;1M\x1b[<0;1;1m')
            drain(.1)
            assert popup.poll() is None and focused(window=windows[0]), (kind, 'outside drag activated or closed')
            os.write(master, b'\x1b[<0;1;1M')
            drain(.1)
            assert popup.poll() is None, (kind, 'closed before release')
            os.write(master, b'\x1b[<0;1;1m')
            popup.wait(timeout=5)
            assert popup.returncode == 0 and focused(window=windows[0]), (kind, popup.stderr.read())
            assert tmux('show-option', '-gqv', '@backdrop-leaked') == '', kind
            drain(.15)
        tmux('unbind-key', '-n', 'MouseDown1Pane')
        print('PASS Sidebar and every filtered list dismiss on outside release without click-through', flush=True)
        # Compatibility pick entrypoints use the same searchable touch menu,
        # with the invoking client retained for the eventual focus operation.
        for mode in ('old', 'new'):
            subprocess.check_call([core, 'ui', mode], env=env, stdout=subprocess.DEVNULL)
            for kind in ('session', 'agent'):
                drain()
                popup = subprocess.Popen([core, 'pick', kind],
                                         env=dict(env, WORKBENCH_SOURCE_PANE=source, WORKBENCH_TARGET_CLIENT=client),
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                popups.append(popup)
                text('Switch ' + ('sessions' if kind == 'session' else 'agents'))
                os.write(master, b'other')
                wait(lambda: any('/ other' in row for row in screen_rows()), 'picker query did not render')
                os.write(master, b'\r')
                finish_popup(popup)
                assert popup.returncode == 0, popup.stderr.read()
                wait(lambda: focused(session='other', pane=other), 'compatibility picker lost the target client')
                assert client_value(sibling, '#{session_name}') == 'source'
                reset()
        print('PASS public Session/Agent pickers keep search, exact focus and client isolation in both menu modes', flush=True)
        # Both lines of a Sidebar entry must activate the same window, and
        # press-and-hold must not activate before release.
        for part in (0, 1):
            popup = open_list('window')
            y = first_row() + 2 + part
            press(10, y)
            drain(.35)
            assert focused(window=windows[0]), 'press activated before release'
            release(10, y)
            popup.wait(timeout=5)
            assert popup.returncode == 0, popup.stderr.read()
            wait(lambda: focused(window=windows[1]), 'card line did not select current-session window')
            reset()
        print('PASS both Sidebar entry rows select on release inside the bordered modal', flush=True)
        tmux('set-option', '-g', 'status', 'on')
        popup = open_list('window')
        tap(10, first_row() + 2)
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[1]), 'status bar changed the popup hit target')
        reset()
        tmux('set-option', '-g', 'status', 'off')
        print('PASS bordered navigation keeps the correct target with the status bar visible', flush=True)
        popup = open_list('window')
        wheel(True, 6)
        wait(lambda: 'window-zero' not in visible_rows()[first_row() - 1], 'wheel did not scroll the window list')
        before_refresh = visible_rows()[first_row() - 1]
        drain(1.4)
        assert visible_rows()[first_row() - 1] == before_refresh, 'refresh reset the wheel position'
        assert focused(window=windows[0]) and popup.poll() is None, 'wheel activated a target'
        wheel(False, 8)
        wait(lambda: 'window-zero' in visible_rows()[first_row() - 1], 'wheel up did not stop at the beginning')
        wheel(True)
        drain(.2)
        tap(10, first_row() + 2)
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[1]), 'click after wheel scrolling chose the wrong window')
        reset()
        print('PASS shared wheel scrolling preserves position through refresh and keeps subsequent clicks accurate', flush=True)
        popup = open_list('window')
        footer(2)
        drain(.15)
        footer(2)
        text('window-8')
        footer(1)
        drain(.15)
        footer(1)
        text('window-zero')
        search_list('window-8')
        wait(lambda: 'window-8' in visible_rows()[first_row() - 1], 'window search result did not render')
        os.write(master, b'\r')
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[8]), 'search result did not select last window')
        reset()
        print('PASS touch paging and search reach windows absent from the status bar', flush=True)
        popup = open_list('window')
        press(10, first_row() + 3)
        x, y = position(10, first_row())
        os.write(master, f'\x1b[<32;{x};{y}M'.encode())
        release(10, first_row())
        drain(.2)
        assert focused(window=windows[0]) and popup.poll() is None, 'swipe also activated a card'
        close(popup)
        popup = open_list('pane')
        output = drain(.2)
        search_list('hidden-sidebar')
        wait(lambda: any('No matching results' in row for row in visible_rows()), 'hidden Sidebar matched the pane query')
        os.write(master, b'\x15')
        wait(lambda: any('pane-right' in row for row in visible_rows()), 'cleared pane query did not restore its targets')
        tap(10, first_row() + 3)
        popup.wait(timeout=5)
        wait(lambda: focused(pane=second), 'pane card did not focus the pane')
        reset()
        popup = open_list('session')
        tap(10, 5)
        popup.wait(timeout=5)
        wait(lambda: focused(pane=other, session='other'), 'session card did not restore its active target')
        assert client_value(sibling, '#{session_name}') == 'source'
        reset()
        popup = open_list('agent')
        tap(10, 5)
        popup.wait(timeout=5)
        wait(lambda: focused(pane=other, session='other'), 'Agent list did not focus the actual Agent')
        assert client_value(sibling, '#{session_name}') == 'source'
        reset()
        print('PASS independent session, Agent and pane views, Sidebar exclusion and client isolation', flush=True)
        popup = open_list('window')
        press(10, first_row() + 2)
        tmux('kill-window', '-t', windows[1])
        drain(1.5)
        release(10, first_row() + 2)
        drain(.2)
        assert focused(window=windows[0]) and popup.poll() is None, 'refresh retargeted a held touch'
        close(popup)
        for width, height in ((63, 31), (160, 50)):
            resize(width, height)
            popup = open_list('window')
            _, _, inner_width, inner_height = content_rect()
            assert inner_width + 2 < width and inner_height + 2 < height
            tap(10, first_row() + 2)
            popup.wait(timeout=5)
            wait(lambda: focused(window=windows[2]), 'bounded Sidebar popup click missed its target')
            reset()
        print('PASS phone and desktop popups leave terminal context visible and share accurate hit regions', flush=True)
        for width, height in ((32, 12), (24, 8)):
            resize(width, height)
            popup = open_list('window')
            footer(2)
            drain(.2)
            tap(10, first_row())
            popup.wait(timeout=5)
            target = windows[2]
            wait(lambda: focused(window=target), 'small viewport card was not usable')
            reset()
        resize(40, 20)
        popup = open_list('window')
        search_list('window-8')
        wait(lambda: 'window-8' in visible_rows()[first_row() - 1], 'window search result did not render before resize')
        resize(24, 8)
        tap(10, first_row())
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[8]), 'keyboard-sized resize lost the selected search result')
        reset()
        resize(40, 20)
        print('PASS disappearing targets and 32/24-column viewports never misroute a touch', flush=True)
        popup = open_list('window')
        footer(0)
        popup.wait(timeout=5)
        text('Session')
        os.write(master, b'w')
        text('Switch windows')
        footer(3)
        drain(.6)
        assert focused(window=windows[0])
        assert tmux('display-message', '-p', '-t', source, '#{pane_in_mode}') == '0'
        print('PASS Switch submenu reopens its touch list and Close leaves native tree mode unused', flush=True)
        # Sidebar keyboard and contextual-menu entries use the same lists,
        # while Back returns to Sidebar rather than an unrelated tmux menu.
        prepare_popup()
        sidebar = subprocess.Popen([str(Path(__file__).resolve().parents[1] / 'bin/tmux-agent-workbench-cli'), 'popup'],
                                   env=dict(env, WORKBENCH_SOURCE_PANE=source, WORKBENCH_TARGET_CLIENT=client),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(sidebar)
        text('+ new')
        drain(.2)
        os.write(master, b's')
        text('Switch sessions')
        footer(0)
        text('+ new')
        drain(.2)
        tap(content_rect()[2] - 2, 1)
        text('workbench')
        os.write(master, b'\x7f')
        text('+ new')
        tap(content_rect()[2] - 2, 1)
        text('workbench')
        os.write(master, b'a')
        text('Switch agents')
        footer(0)
        text('+ new')
        os.write(master, b'\x1b')
        drain(.7)
        assert focused(window=windows[0])
        print('PASS Sidebar and contextual-menu routes share the touch UI and preserve their Back destination', flush=True)
        # A lower current Agent must not open the overview halfway through its
        # groups. The Agents/sort heading also stays usable after scrolling.
        prepare_popup()
        lower_pane = tmux('display-message', '-p', '-t', windows[8], '#{pane_id}')
        sidebar = subprocess.Popen([core, 'sidebar', '--popup', '--pane', lower_pane, '--client', client],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(sidebar)
        text('+ new')
        wait(lambda: 'agents' in visible_rows()[1] and 'grouped' in visible_rows()[1]
             and visible_rows()[3].strip() == 'other', 'Sidebar hid its heading or first Session group')
        for _ in range(2):
            wheel(True, 12)
            wait(lambda: visible_rows()[3].strip() != 'other', 'first Session group did not scroll away')
            wheel(False, 50)
            wait(lambda: visible_rows()[3].strip() == 'other', 'wheel could not restore the first Session group heading')
            drain(1.4)
            assert visible_rows()[3].strip() == 'other', 'snapshot refresh hid the restored first group'
        assert 'agents' in visible_rows()[1] and 'grouped' in visible_rows()[1], visible_rows()
        assert visible_rows()[2].strip('─ ') == '' and '─' in visible_rows()[2], visible_rows()
        assert visible_rows()[-2].strip('─ ') == '' and '─' in visible_rows()[-2], visible_rows()
        tap(content_rect()[2] - 2, 2)
        text('prioritized')
        assert 'agents' in visible_rows()[1] and 'prioritized' in visible_rows()[1], visible_rows()
        tap(content_rect()[2] - 2, 2)
        text('grouped')
        wait(lambda: visible_rows()[3].strip() == 'other', 'grouped sort did not restore the first group')
        os.write(master, b'\x1b')
        sidebar.wait(timeout=5)
        drain(.7)
        assert focused(window=windows[0]), 'opening the overview changed its source window'
        print('PASS overview pins its Agents/sort header and restores the first group after repeated wheel round trips', flush=True)
        # A shared pane ID must still list and focus windows in this client's
        # Session rather than whichever Session tmux finds first.
        tmux('new-session', '-d', '-s', 'linked', '-n', 'linked-only', '/bin/sh')
        tmux('link-window', '-s', 'source:0', '-t', 'linked:5')
        tmux('switch-client', '-c', client, '-t', 'linked:0')
        popup = open_list('window')
        search_list('window-8')
        wait(lambda: any('No matching results' in row for row in visible_rows()), 'linked window query escaped its session')
        os.write(master, b'\x15')
        wait(lambda: any('linked-only' in row for row in visible_rows()), 'cleared linked query did not restore its target')
        tap(10, first_row() + 3)
        popup.wait(timeout=5)
        wait(lambda: focused(window=windows[0], session='linked'), 'linked window chose another Session')
        assert client_value(sibling, '#{session_name}') == 'source'
        reset()
        tmux('kill-session', '-t', 'linked')
        print('PASS linked windows preserve the invoking Session and client', flush=True)
        tmux('respawn-pane', '-k', '-t', other, '/bin/sh')
        popup = open_list('agent')
        wait(lambda: any('No running agents' in row for row in visible_rows()), 'empty Agent state did not render')
        tap(10, first_row())
        assert popup.poll() is None and focused(window=windows[0])
        close(popup)
        print('PASS an empty Agent view stays usable and never turns a blank row into a target', flush=True)
        # The overview Sidebar uses the same wheel/drag controller as filtered
        # views, including when the pointer is over the pinned action bar.
        for index in range(8):
            tmux('new-session', '-d', '-s', f'wheel-{index}', '/bin/sh')
        drain(1.3)
        prepare_popup()
        sidebar = subprocess.Popen([core, 'sidebar', '--popup', '--pane', source, '--client', client],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(sidebar)
        text('+ new')
        wait(lambda: any('sessions' in row for row in visible_rows()), 'Sidebar session rows not ready')
        wheel(True, 20)
        wait(lambda: any('wheel-7' in row for row in visible_rows()), 'Sidebar wheel did not reach the last session')
        after_scroll = visible_rows()[1:-1]
        drain(1.4)
        assert visible_rows()[1:-1] == after_scroll, 'Sidebar refresh pulled the wheel back to its selection'
        assert focused(window=windows[0]), 'Sidebar wheel changed the client target'
        wheel(False, 20)
        drain(.2)
        assert not any('wheel-7' in row for row in visible_rows()), 'Sidebar wheel could not return to the start'
        for _ in range(12):
            if any('wheel-0' in row for row in visible_rows()):
                break
            wheel(True)
            drain(.1)
        assert any('wheel-0' in row for row in visible_rows()), 'Sidebar could not browse to a session row'
        row = next(index + 1 for index, value in enumerate(visible_rows()) if 'wheel-0' in value)
        press(10, row)
        drain(.2)
        assert focused(window=windows[0]), 'Sidebar activated before release'
        wheel(True)
        release(10, row)
        drain(.2)
        assert sidebar.poll() is None and focused(window=windows[0]), 'wheeling while held also activated the Sidebar row'
        close(sidebar)
        print('PASS overview Sidebar shares wheel limits, refresh stability and release cancellation with filtered lists', flush=True)
        subprocess.check_call([core, 'ui', 'old'], env=env, stdout=subprocess.DEVNULL)
        prepare_popup()
        sidebar = subprocess.Popen([core, 'sidebar', '--popup', '--pane', source, '--client', client],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        popups.append(sidebar)
        text('+ new')
        drain(.2)
        tap(content_rect()[2] - 2, 1)
        text('workbench')
        os.write(master, b'\x7f')
        text('+ new')
        os.write(master, b's')
        text('Switch sessions')
        os.write(master, b'\x7f')
        text('+ new')
        os.write(master, b'\x1b')
        sidebar.wait(timeout=5)
        drain(.7)
        print('PASS classic menu uses the shared switcher and preserves its Sidebar Back destination', flush=True)
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
