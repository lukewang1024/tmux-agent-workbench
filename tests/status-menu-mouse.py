#!/usr/bin/env python
"""Check real menu mouse dismissal and shortcuts through a tmux client PTY."""
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid

from daemon_fixture import running_daemon

repo = Path(__file__).resolve().parents[1]
core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='wb-menu-mouse-') as root, ExitStack() as processes:
    root = Path(root)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_LANGUAGE='en', TMUX_AGENT_WORKBENCH_BIN=core,
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
    for key in ('TMUX', 'TMUX_PANE'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = str(root / key)
    shim = root / 'bin'
    shim.mkdir()
    hosts = root / 'hosts'
    hosts.write_text('dev-host\ntest-host\n')
    host_connected = root / 'host-connected'
    ssh = shim / 'ssh-connect'
    ssh.write_text('#!/bin/sh\ncase "$1" in hosts) cat ' + shlex.quote(str(hosts)) +
                   ' ;; connect) printf "%s\\n" "$2" > ' + shlex.quote(str(host_connected)) +
                   '; exec sleep 100000 ;; esac\n')
    ssh.chmod(0o755)
    # Keep launch fixtures long-lived without calling any model API. The
    # Node-based fixture below is used when process identity must be detected.
    codex = shim / 'codex'
    codex.write_text('#!/bin/sh\nexec node "$0.js" "$@"\n')
    codex.chmod(0o755)
    codex_js = shim / 'codex.js'
    codex_js.write_text('process.stdout.write("interactive-ready\\n"); setInterval(() => {}, 1000000);\n')
    for tool in ('claude', 'opencode'):
        (shim / tool).symlink_to(codex)
    preview = root / 'preview.py'
    preview.write_text('import json, sys\na = sys.argv[1:]\ntool = a[1]\nscene = a[a.index("--scene") + 1] if "--scene" in a else None\nteam = "--team" in a\ncfg = {"model": scene + ("-lead" if team else "-single"), "effort": "high", "worker_model": "worker-model", "worker_effort": "max", "fast": scene == "daily"} if scene else {}\nprint(json.dumps({"config": cfg}))\n')
    # Panel previews read the same declarative settings as agent-team. Any
    # --dry-run process on the interaction path is a regression, including
    # the very first agent selection with an empty cache.
    settings_dir = root / 'config'
    settings_dir.mkdir()
    scenes = {scene: {'model': scene + '-single', 'lead_model': scene + '-lead',
                     'effort': 'high', 'worker_model': 'worker-model',
                     'worker_effort': 'max', 'fast': scene == 'daily'}
              for scene in ('daily', 'hard', 'batch')}
    (settings_dir / 'teams.json').write_text(json.dumps({
        'codex': {'scenes': scenes}, 'traex': {'scenes': scenes},
        'claude': {'team': {}}, 'opencode': {'team': {}}}))
    preview_calls = root / 'preview-calls'
    preview.write_text('from pathlib import Path\nwith Path(' + repr(str(preview_calls)) + ').open("a") as calls: calls.write("preview\\n")\n' + preview.read_text())
    preview_handler = 'case " $* " in *" --dry-run "*) exec ' + shlex.quote(sys.executable) + ' ' + shlex.quote(str(preview)) + ' "$@" ;; esac\n'
    reject_scene = root / 'reject-scene'
    team_cli = shim / 'agent-team'
    team_cli.write_text('#!/bin/sh\n' + preview_handler + 'case " $* " in *" --check "*) if [ -f ' + shlex.quote(str(reject_scene)) + ' ]; then echo "Configured model is unavailable" >&2; exit 1; fi; exit 0 ;; esac\nexec ' + shlex.quote(str(codex)) + ' "$@"\n')
    team_cli.chmod(0o755)
    # Source selection reopens Usage in a background tmux callback. Track
    # that callback's completion instead of assuming a fixed sleep closes it.
    usage_choice_done = root / 'usage-choice-done'
    usage_wrapper = shim / 'workbench-agent-usage'
    usage_wrapper.write_text('#!/bin/sh\n' + shlex.quote(str(repo / 'bin/workbench-agent-usage')) +
                             ' "$@"\nresult=$?\nif [ "${1:-}" = choose ]; then : > ' +
                             shlex.quote(str(usage_choice_done)) + '; fi\nexit "$result"\n')
    usage_wrapper.chmod(0o755)
    env['PATH'] = str(shim) + ':' + str(repo / 'bin') + ':' + env['PATH']
    # Give only the legacy Host menu a deliberately long machine name. Keep
    # the native tmux menu and client real so an oversized title cannot pass
    # merely because this particular runner happens to have a short hostname.
    legacy_bin = root / 'long-host-bin'
    legacy_bin.mkdir()
    legacy_tmux = legacy_bin / 'tmux.py'
    legacy_tmux.write_text(
        'import os, sys\n'
        'args = [arg.replace("#H", "#{@workbench_test_hostname}")'
        '.replace(":host}", ":@workbench_test_hostname}") for arg in sys.argv[1:]]\n'
        'os.execv(' + repr(shutil.which('tmux', path=env['PATH'])) + ', ["tmux", *args])\n')
    legacy_wrapper = legacy_bin / 'tmux'
    legacy_wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(sys.executable) + ' ' +
                              shlex.quote(str(legacy_tmux)) + ' "$@"\n')
    legacy_wrapper.chmod(0o755)
    legacy_path = str(legacy_bin) + ':' + env['PATH']
    project = root / 'project'
    for name in ('grill-me', 'handoff'):
        skill = project / '.agents/skills' / name
        skill.mkdir(parents=True)
        (skill / 'SKILL.md').write_text(f'---\nname: {name}\ndescription: Menu fixture\n---\n')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def drain(seconds=0.15):
        output = b''
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([master], [], [], max(0, end - time.monotonic()))
            if ready:
                output += os.read(master, 65536)
        return output

    def wait_for_pane_text(expected, target):
        # Menu callbacks run via run-shell -b after the menu process exits.
        deadline = time.monotonic() + 5
        while True:
            content = tmux('capture-pane', '-p', '-t', target)
            if expected in content:
                return
            if time.monotonic() >= deadline:
                raise AssertionError(('menu action did not reach pane', expected, target, content))
            drain(0.05)

    def wait_for_agent_actions(target, expected):
        deadline = time.monotonic() + 5
        while tmux('display-message', '-p', '-t', target, '#{@workbench_agent_actions}') != expected:
            if time.monotonic() >= deadline:
                raise AssertionError(('Agent actions flag did not update', target, expected, tmux('list-panes', '-a', '-F', '#{pane_id}|#{pane_current_command}|#{@workbench_agent_actions}|#{@workbench_window_has_agent}')))
            drain(0.05)

    def wait_for_pane_command(target, expected):
        # A window exists before its shell execs the requested command. This is
        # especially visible on macOS release runners; wait for readiness, not
        # just window creation, before checking launch arguments or cwd.
        deadline = time.monotonic() + 5
        while True:
            actual = tmux('display-message', '-p', '-t', target, '#{pane_current_command}')
            if actual in ((expected,) if isinstance(expected, str) else expected):
                return
            if time.monotonic() >= deadline:
                raise AssertionError(('launched command did not become ready', target, expected,
                                      actual, tmux('capture-pane', '-p', '-t', target)))
            drain(0.05)

    def screen_rows(output):
        # Project cursor writes and erases onto the current terminal screen.
        columns, rows = os.get_terminal_size(slave)
        screen = {}
        x = y = 0
        tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b[ -/]*[0-~]|[^\x1b]', output.decode(errors='replace'))
        for token in tokens:
            if token.startswith('\x1b['):
                command = token[-1]
                values = token[2:-1]
                if any(char not in '0123456789;' for char in values):
                    continue
                numbers = [int(value or 0) for value in values.split(';')]
                amount = numbers[0] or 1
                if command in ('H', 'f'):
                    y = amount - 1
                    x = (numbers[1] or 1) - 1 if len(numbers) > 1 else 0
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
            elif token.isprintable():
                screen[x, y] = token
                x += 1
        return [''.join(screen.get((col, row), ' ') for col in range(columns)) for row in range(rows)]

    def screen_text(output):
        return '\n'.join(screen_rows(output)).encode()

    def item_position(output, label="close"):
        rows = screen_rows(output)
        for row, text in enumerate(rows):
            if label in text:
                return text.index(label) + 2, row + 1
        raise AssertionError(('Menu item position missing', label, rows))

    def open_menu(*args, **kwargs):
        # Drain the previous screen before launching, so its Close footer
        # cannot satisfy the new menu readiness check.
        drain()
        return subprocess.Popen(*args, **kwargs)

    def wait_for_text(proc, needle=b'close', *additional, current_screen=True, absent=(), menu_footer=True):
        output = b''
        for _ in range(40):
            output += drain(0.05)
            visible = '\n'.join(screen_rows(output)).encode() if current_screen else output
            # Native menu content precedes its Close footer. A matching heading
            # alone can arrive before later rows, especially on slower clients.
            expected = (needle, *additional) + ((b'(esc)',) if menu_footer else ())
            if all(text in visible for text in expected) and not any(text in visible for text in absent):
                return output
            if proc.poll() is not None:
                probe = subprocess.run(
                    ['tmux', '-S', socket, 'list-clients', '-F', '#{client_name} #{client_tty}'],
                    env=env, text=True, capture_output=True, check=False,
                )
                raise AssertionError((
                    'menu exited before rendering', proc.returncode, proc.stderr.read(),
                    'tmux probe', probe.returncode, probe.stdout, probe.stderr,
                ))
        raise AssertionError(('menu did not render', visible))

    def write_frame(proc, message):
        payload = json.dumps(message, separators=(',', ':')).encode()
        proc.stdin.write(len(payload).to_bytes(4, 'big') + payload)
        proc.stdin.flush()

    def read_frame(proc):
        header = proc.stdout.read(4)
        if len(header) != 4:
            raise AssertionError(('client protocol closed before reply',
                                  proc.poll(), proc.stderr.read().decode(errors='replace')))
        length = int.from_bytes(header, 'big')
        payload = proc.stdout.read(length)
        if len(payload) != length:
            raise AssertionError(('truncated client protocol reply', payload))
        return json.loads(payload)

    def register_termux():
        proc = subprocess.Popen([core, 'client', 'serve'], env=env,
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        write_frame(proc, {
            'type': 'hello', 'version': 2, 'device_id': str(uuid.uuid4()),
            'terminal_id': str(uuid.uuid4()), 'device_label': 'test-phone',
            'kind': 'termux', 'capabilities': [],
        })
        welcome = read_frame(proc)
        assert welcome['type'] == 'welcome', welcome
        attach_env = dict(env, SSH_TTY=os.ttyname(slave))
        try:
            subprocess.run([core, 'client', 'attach-pty', '--bind', welcome['attachment_token']],
                           env=attach_env, stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=3)
        except subprocess.TimeoutExpired:
            pass
        return proc

    client_process = None
    menu = None
    termux = None
    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'audit', '-x', '80', '-y', '30', '-c', str(project), '/bin/sh')
        # Keep the fixture server alive while the PTY client is being attached
        # and while popup processes briefly detach/reconnect. Without these
        # explicit lifecycle settings, tmux can reap the temporary server in
        # the gap between two asynchronous menu actions, making the following
        # action fail with "no server running" instead of testing the menu.
        tmux('set-option', '-g', 'exit-empty', 'off')
        tmux('set-option', '-g', 'exit-unattached', 'off')
        tmux('set-option', '-g', 'default-shell', '/bin/sh')
        tmux('set-option', '-g', 'status-keys', 'emacs')
        tmux('set-option', '-g', 'mouse', 'on')
        # Retain the native Escape parser timeout; wait for it after dismissal.
        tmux('set-option', '-sg', 'escape-time', '500')
        tmux('set-option', '-g', 'status', 'off')
        tmux('set-option', '-g', '@workbench-usage-source', 'opencode')
        server_pid = tmux('display-message', '-p', '#{pid}')
        env['TMUX'] = f'{socket},{server_pid},0'
        env['TMUX_AGENT_WORKBENCH_TMUX_SOCKET'] = socket
        pane = tmux('display-message', '-p', '#{pane_id}')
        client_process = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'audit'],
                                          stdin=slave, stdout=slave, stderr=slave, env=env)
        for _ in range(40):
            client = tmux('list-clients', '-F', '#{client_name}')
            if client:
                break
            drain(0.05)
        drain()
        version = re.search(r'(\d+)\.(\d+)', tmux('display-message', '-p', '#{version}'))
        release_menus = version is not None and tuple(map(int, version.groups())) >= (3, 8)
        root_args = [core, 'status-menu', 'tmux', '--pane', pane, '--client', client]
        menu = open_menu(root_args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu, b'Switch window', b'Switch session', b'New session...', b'Launch agent...')
        labels = ('Switch window', 'Switch session', 'Switch agent', 'Switch pane', 'New session...', 'New window', 'Launch agent...', 'Panes & layout...')
        positions = [item_position(rendered, label)[1] for label in labels]
        assert positions == sorted(set(positions)), (positions, rendered)
        assert b'Switch window / session' not in screen_text(rendered)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        drain(0.7)
        before_sessions = tmux('list-sessions', '-F', '#{session_id}')
        menu = open_menu(root_args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'New session...')
        os.write(master, b'n')
        wait_for_text(client_process, b'New session name:', menu_footer=False)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        drain(0.7)
        assert tmux('list-sessions', '-F', '#{session_id}') == before_sessions
        # A second attached client must stay in its original session while
        # the invoking client creates a named session in the source directory.
        def client_session(name):
            return dict(line.split('\t', 1) for line in tmux(
                'list-clients', '-F', '#{client_name}\t#{session_name}').splitlines())[name]

        other_master, other_slave = pty.openpty()
        other_client_process = None
        try:
            fcntl.ioctl(other_slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
            other_client_process = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'audit'],
                                                    stdin=other_slave, stdout=other_slave, stderr=other_slave, env=env)
            for _ in range(40):
                other_clients = [name for name in tmux('list-clients', '-F', '#{client_name}').splitlines() if name != client]
                if other_clients:
                    break
                drain(0.05)
            assert len(other_clients) == 1, other_clients
            for session_name in ("new 'session'; display-message injected", '中文会话'):
                menu = open_menu(root_args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                wait_for_text(menu, b'New session...')
                os.write(master, b'n')
                wait_for_text(client_process, b'New session name:', menu_footer=False)
                os.write(master, session_name.encode() + b'\r')
                menu.wait(timeout=3)
                for _ in range(40):
                    if client_session(client) == session_name:
                        break
                    drain(0.05)
                assert client_session(client) == session_name, (
                    session_name, tmux('list-sessions', '-F', '#{session_id} #{session_name}'),
                    tmux('list-clients', '-F', '#{client_name} #{session_name}'))
                created = next(sid for sid, name in (
                    line.split('\t', 1) for line in tmux('list-sessions', '-F', '#{session_id}\t#{session_name}').splitlines()
                ) if name == session_name)
                assert tmux('display-message', '-p', '-t', created + ':', '#{pane_current_path}') == str(project)
                assert client_session(other_clients[0]) == 'audit'
                tmux('switch-client', '-c', client, '-t', 'audit')
                tmux('kill-session', '-t', created)
                drain()
        finally:
            if other_client_process:
                other_client_process.terminate()
                other_client_process.wait(timeout=5)
            os.close(other_master)
            os.close(other_slave)
        print('PASS root switch entries and named session creation, cancellation, cwd and client isolation')
        # The combined entry is deliberately smaller than Sidebar, with the
        # same release/scroll interaction. Exercise the key sequence emitted
        # by Termux's SSH swipe-up macro as well as all five destinations.
        tmux('set-option', '-g', '@workbench_plugin_dir', str(repo))
        tmux('set-option', '-g', 'prefix', '`')
        tmux('bind-key', '-T', 'prefix', 'M-m', 'run-shell', '-b', shlex.join(
            [str(repo / 'bin/tmux-agent-workbench-cli'), 'menu', '--pane', pane, '--client', client]))
        os.write(master, b'`\x1bm')
        rendered = wait_for_text(client_process, b'Sidebar', b'Host / SSH', b'CPU / resources')
        borders = [row for row in screen_rows(rendered) if '┌' in row and '┐' in row]
        assert len(borders) == 1 and borders[0].index('┐') - borders[0].index('┌') < 60, borders
        for key, expected in [(b'a', b'No foreground agent'), (b'h', b'dev-host'),
                              (b'c', b'Resources'), (b't', b'New session...')]:
            os.write(master, key)
            wait_for_text(client_process, expected, b'(back)')
            os.write(master, b'\x7f')
            rendered = wait_for_text(client_process, b'Sidebar', b'CPU / resources')
        # A scoped Sidebar view returns to its collection, then the main menu.
        x, y = item_position(rendered, 'Sidebar')
        os.write(master, f'\x1b[<0;{x};{y + 1}M\x1b[<0;{x};{y + 1}m'.encode())
        wait_for_text(client_process, b'Open Sidebar')
        os.write(master, b'w')
        wait_for_text(client_process, b'Switch windows', menu_footer=False)
        os.write(master, b'\x7f')
        wait_for_text(client_process, b'Open Sidebar', b'(back)')
        os.write(master, b'\x7f')
        rendered = wait_for_text(client_process, b'Sidebar', b'CPU / resources')
        x, y = item_position(rendered, 'close')
        os.write(master, f'\x1b[<0;{x};{y + 1}M\x1b[<0;{x};{y + 1}m'.encode())
        drain(.7)
        tmux('set-option', '-g', 'prefix', 'C-b')
        # More than 35 hosts remain reachable. Wheel scrolling over the pinned
        # footer must cancel a held button, and a second-row tap uses the item
        # currently painted there, without retaining an old index.
        hosts.write_text(''.join(f'host-{index:02}\n' for index in range(50)))
        menu = open_menu([core, 'status-menu', 'host', '--pane', pane, '--client', client],
                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu, b'host-00')
        x, y = item_position(rendered, 'host-00')
        footer_x, footer_y = item_position(rendered, 'close')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<65;{footer_x};{footer_y}M\x1b[<0;{x};{y}m'.encode())
        rendered += drain(.2)
        assert menu.poll() is None and not host_connected.exists()
        for _ in range(48):
            os.write(master, f'\x1b[<65;{footer_x};{footer_y}M'.encode())
            rendered += drain(.015)
        visible = '\n'.join(screen_rows(rendered))
        assert 'host-48' in visible and 'host-00' not in visible, visible
        x, y = item_position(rendered, 'host-48')
        os.write(master, f'\x1b[<0;{x};{y + 1}M\x1b[<0;{x};{y + 1}m'.encode())
        menu.wait(timeout=3)
        deadline = time.monotonic() + 5
        while not host_connected.exists():
            assert time.monotonic() < deadline, 'scrolled host action did not execute'
            drain(.05)
        assert host_connected.read_text() == 'host-48\n'
        host_window = tmux('display-message', '-p', '-c', client, '#{window_id}')
        tmux('kill-window', '-t', host_window)
        hosts.write_text('dev-host\ntest-host\n')
        drain(.3)
        # All private requests/replies are deleted after their popup closes.
        assert not list(root.glob('**/menu-*.json')) and not list(root.glob('**/menu-*.reply'))
        print('PASS compact menu collection, Termux key sequence, nested Back, wheel scrolling and large button hit areas')
        if release_menus:
            for input_mode in ('mouse', 'touch'):
                menu = open_menu([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                                  '--input-mode', input_mode], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                rendered = wait_for_text(menu) + drain()
                for label, expected in (
                    ('Launch agent...', b'choose agent'),
                    ('Codex', b'daily-single'),
                    ('Batch', b'batch-single'),
                    ('\u2190 Back', b'choose agent'),
                ):
                    x, y = item_position(rendered, label)
                    if input_mode == 'mouse':
                        # Hover inside and outside must not select or dismiss.
                        os.write(master, b'\x1b[<35;1;1M')
                        drain(0.1)
                        os.write(master, f'\x1b[<35;{x};{y}M'.encode())
                        drain(0.1)
                    os.write(master, f'\x1b[<0;{x};{y}M'.encode())
                    held = drain(0.6)
                    assert expected not in held, (input_mode, label, 'selected before release', held)
                    os.write(master, f'\x1b[<0;{x};{y}m'.encode())
                    rendered = wait_for_text(client_process, expected) + drain(0.3)
                # The final release must leave the chooser usable, not close it.
                os.write(master, b'x')
                wait_for_text(client_process, b'daily-single')
                os.write(master, b'\x1b')
                drain(0.7)
                menu.wait(timeout=3)
                print('PASS held clicks navigate and redraw exactly once on release:', input_mode)
        tmux('set-option', '-g', '@workbench_test_hostname', 'long-ci-host-' + 'x' * 100)
        for kind in ('host', 'tmux', 'agent', 'usage', 'metrics', 'legacy-host'):
            for close in ('outside', 'escape', 'backspace', 'button'):
                args = ([str(repo / 'bin/workbench-agent-usage'), 'menu', client] if kind == 'usage'
                        else [str(repo / 'bin/workbench-host-metrics-menu'), client, pane] if kind == 'metrics'
                        else [str(repo / 'bin/workbench-host-menu'), client, pane] if kind == 'legacy-host'
                        else [str(repo / 'bin/workbench-status-popup'), kind, client, pane])
                menu = open_menu(args, env=dict(env, PATH=legacy_path) if kind == 'legacy-host' else env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                rendered = wait_for_text(menu)
                assert b'(back)' not in screen_text(rendered) and b'(BSpace)' not in screen_text(rendered), (kind, rendered)
                if kind == 'host':
                    # Native menu titles are bounded by the terminal width.
                    # CI hosts can have names longer than the 80-column client;
                    # short hostnames must appear in full, long ones by prefix.
                    hostname = tmux('display-message', '-p', '#{host}')
                    assert hostname[:24].encode() in screen_text(rendered), rendered
                elif kind == 'legacy-host':
                    assert b'long-ci-host-' in screen_text(rendered), rendered
                if close == 'outside':
                    os.write(master, b'\x1b[<0;1;1M\x1b[<0;1;1m')
                    drain(.2)
                    assert menu.poll() is None, 'outside tap dismissed the touch menu'
                    os.write(master, b'\x1b')
                elif close == 'escape':
                    os.write(master, b'\x1b')
                elif close == 'backspace':
                    os.write(master, b'\x7f')
                else:
                    x, y = item_position(rendered)
                    os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
                for _ in range(100):
                    drain(0.05)
                    if menu.poll() is not None:
                        break
                assert menu.poll() == 0, (kind, close, menu.poll())
                assert tmux('list-windows', '-F', '#{window_id}').count('\n') == 0, (kind, close)
                if close == 'escape':
                    drain(0.7)
                print('PASS', kind, close)
        # Every submenu uses the same navigation, including Configure and
        # views with no available actions. Escape must never open its parent.
        for view in ('launch', 'configure', 'panes', 'more', 'team', 'botmux', 'agent-target'):
            args = [core, 'status-menu', 'tmux', '--pane', pane, '--client', client, '--view', view]
            parent = b'choose agent' if view == 'configure' else b'Launch agent...'
            menu = open_menu(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            rendered = wait_for_text(menu) + drain()
            assert b'(back)' in screen_text(rendered) and b'(esc)' in screen_text(rendered), (view, rendered)
            assert not any(key in screen_text(rendered) for key in (b'(BSpace)', b'(Escape)', b'(Enter)')), (view, rendered)
            back_x, back_y = item_position(rendered, '← Back')
            close_x, close_y = item_position(rendered, 'close')
            assert close_y == back_y
            back_key_x, _ = item_position(rendered, '(back)')
            close_key_x, _ = item_position(rendered, '(esc)')
            assert item_position(rendered, '(back)')[1] == back_y + 1
            assert item_position(rendered, '(esc)')[1] == close_y + 1
            if view == 'configure':
                launch_x, launch_y = item_position(rendered, '▶ Launch')
                assert launch_y + 2 <= back_y
                enter_key_x, _ = item_position(rendered, '(enter)')
                assert item_position(rendered, '(enter)')[1] == launch_y
            assert b'(B)' not in screen_text(rendered), (view, 'legacy Back shortcut')
            os.write(master, b'\x7f')
            menu.wait(timeout=3)
            wait_for_text(client_process, parent)
            os.write(master, b'\x1b')
            drain(0.7)
            menu = open_menu(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_for_text(menu)
            drain()
            os.write(master, b'\x1b')
            menu.wait(timeout=3)
            assert parent not in drain(0.7), (view, 'Escape opened the parent')
        print('PASS every submenu: Backspace returns, Escape closes, aligned shared footer')
        # Exercise the status binding while this PTY represents a desktop
        # pointer. Termux's release-based selection is tested below.
        # Exercise the real status binding block: opening press/release must not
        # activate the first row, then mouse motion and click select one action.
        import shlex
        source = (repo / 'workbench.tmux').read_text()
        bindings = source[source.index('tmux bind-key -T root MouseDown1Status if-shell'):]
        bindings = bindings[:bindings.index('# If the optional theme')]
        action = shlex.join([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane])
        route = ('wb-status-route=if-shell -F '
                 + shlex.quote('#{==:#{mouse_status_range},wb_tmux}') + ' '
                 + shlex.quote('run-shell ' + shlex.quote(action)) + ' '
                 + shlex.quote('select-window -t ='))
        tmux('set-option', '-s', 'command-alias[927]', route)
        subprocess.run(['sh', '-c', bindings], env=env, check=True)
        tmux('set-option', '-g', 'status', 'on')
        tmux('set-option', '-g', 'status-left', '')
        tmux('set-option', '-g', 'status-right', '#[range=user|wb_tmux]MENU#[range=]')
        drain()
        before = tmux('list-windows', '-F', '#{window_id}').splitlines()
        original_window = before[0]
        os.write(master, b'\x1b[<0;78;30M\x1b[<0;78;30m')
        rendered = wait_for_text(client_process)
        assert tmux('list-windows', '-F', '#{window_id}').splitlines() == before
        assert tmux('display-message', '-p', '-t', pane, '#{pane_in_mode}') == '0', 'opening status menu left an output pager'
        # The binding launches the status popup asynchronously; wait for its
        # mouse mode and rendering to settle before moving into the menu.
        drain(0.75)
        x, y = item_position(rendered, 'New window')
        os.write(master, f'\x1b[<35;{x};{y}M'.encode())
        drain(0.1)
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        after = before
        for _ in range(40):
            drain(0.05)
            after = tmux('list-windows', '-F', '#{window_id}').splitlines()
            if len(after) > len(before):
                break
        assert len(after) == len(before) + 1, (
            'status touch did not create exactly one window', before, after,
            tmux('capture-pane', '-p', '-t', pane),
            tmux('list-clients', '-F', '#{client_name} #{client_tty}'),
        )
        created = next(window for window in after if window not in before)
        # Killing the window selected by the attached PTY can terminate that
        # client on some tmux/macOS combinations. Move it back first so the
        # temporary window cleanup cannot race the next menu invocation.
        tmux('switch-client', '-c', client, '-t', original_window)
        tmux('kill-window', '-t', created)
        assert tmux('display-message', '-p', '-t', pane, '#{pane_in_mode}') == '0'
        # A normal window-tab click also must not leave an output pager behind.
        other = tmux('new-window', '-d', '-P', '-F', '#{window_id}', '-n', 'touch-target')
        tmux('set-option', '-g', 'window-status-format', '#W')
        tmux('set-option', '-g', 'window-status-current-format', '#W')
        tmux('refresh-client', '-S')
        rendered = drain(0.3)
        x, y = item_position(rendered, 'touch-target')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        drain(0.3)
        assert tmux('display-message', '-p', '#{window_id}') == other
        assert set(tmux('list-panes', '-s', '-F', '#{pane_in_mode}').splitlines()) == {'0'}
        tmux('switch-client', '-c', client, '-t', original_window)
        tmux('kill-window', '-t', other)
        tmux('set-option', '-g', 'status', 'off')
        drain(0.3)
        print('PASS window tab touch leaves no output pager')
        print('PASS status opening release and subsequent mouse action')
        # Register this PTY as a Termux client so following checks use the
        # same no-hover menu path as a real phone attachment.
        processes.enter_context(running_daemon(core, env))
        termux = register_termux()
        expected_mode = 'release' if release_menus else 'touch'
        mouse_mode = 'mouse'
        for _ in range(40):
            mouse_mode = subprocess.check_output(
                [core, 'menu-mouse-mode', '--client', client], env=env, text=True
            ).strip()
            if mouse_mode == expected_mode:
                break
            time.sleep(0.05)
        assert mouse_mode == expected_mode, (
            'Termux attachment was not recognized for the tmux client',
            mouse_mode,
            tmux('display-message', '-p', '-c', client, '#{client_tty}'),
        )
        # A real foreground process identity is required: a shell must not
        # receive slash commands. Node's wrapper detection recognizes the
        # codex.js argument while the process remains alive for the test.
        tmux('respawn-pane', '-k', '-t', pane, shutil.which('node'), str(codex_js))
        wait_for_pane_command(pane, 'node')
        wait_for_pane_text('interactive-ready', pane)
        wait_for_agent_actions(pane, '1')
        tmux('select-pane', '-t', pane, '-T', 'Codex idle')
        drain(0.3)
        # Touch sends a press/release without any preceding hover/motion.
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, '/side')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        drain(0.3)
        wait_for_pane_text('/side', pane)
        tmux('send-keys', '-t', pane, 'C-u')
        print('PASS touch selects action without hover')
        shell = tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', pane, '/bin/sh')
        tmux('select-pane', '-t', shell)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, shell, '--focus-agent'],
                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'/side')
        assert tmux('display-message', '-p', '#{pane_id}') == pane
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        drain(0.7)
        tmux('kill-pane', '-t', shell)
        print('PASS agent status focuses the window agent from a shell and opens commands')

        menu = open_menu([str(repo / 'bin/workbench-host-metrics-menu'), client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, 'Standard')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        for _ in range(40):
            if tmux('show-option', '-gqv', '@workbench-host-metrics-mode') == 'standard':
                break
            drain(0.05)
        assert tmux('show-option', '-gqv', '@workbench-host-metrics-mode') == 'standard'
        print('PASS metrics touch action')
        # The source stays fixed even if the client's active window changes.
        other = tmux('new-window', '-P', '-F', '#{window_id}', '-n', 'unrelated-shell', '/bin/sh')
        drain()
        # A native Agent action still prefills the source pane without Enter.
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'/btw' not in screen_text(rendered)
        assert b'Focus this pane' not in screen_text(rendered) and b'Refresh' not in screen_text(rendered)
        assert b'/side' in screen_text(rendered), (rendered, tmux('capture-pane', '-p', '-t', pane))
        os.write(master, b's')
        menu.wait(timeout=3)
        drain(0.3)
        wait_for_pane_text('/side', pane)
        assert '/side' not in tmux('capture-pane', '-p', '-t', other)
        tmux('kill-window', '-t', other)
        drain()
        print('PASS agent action targets source pane after active window changes')
        for key, expected in [(b'q', '$grill-me'), (b'h', '$handoff')]:
            tmux('send-keys', '-t', pane, 'C-u')
            drain()
            menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            rendered = wait_for_text(menu)
            assert b'/goal' in screen_text(rendered) and b'grill-me' in screen_text(rendered) and b'handoff' in screen_text(rendered)
            assert b'/fork' not in screen_text(rendered) and b'/status' not in screen_text(rendered)
            os.write(master, key)
            menu.wait(timeout=3)
            drain(0.3)
            wait_for_pane_text(expected, pane)
        tmux('send-keys', '-t', pane, 'C-u')
        print('PASS goal and installed pinned skills on main menu; skills prefill only')
        tmux('select-pane', '-t', pane, '-T', '⠋ Codex')
        for _ in range(60):
            snapshot = json.loads(subprocess.check_output([core, 'snapshot', '--json'], env=env))
            if any(a['target']['pane_id'] == pane and a['base_state'] == 'working' for a in snapshot['agents']): break
            drain(0.05)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'working' in screen_text(rendered) and b'grill-me' in screen_text(rendered) and b'handoff' in screen_text(rendered)
        assert b'/side' in screen_text(rendered) and b'/btw' not in screen_text(rendered)
        os.write(master, b'q')
        menu.wait(timeout=3)
        drain(0.3)
        wait_for_pane_text('$grill-me', pane)
        tmux('send-keys', '-t', pane, 'C-u')
        tmux('select-pane', '-t', pane, '-T', 'Codex idle')
        print('PASS working Codex retains skill shortcuts and uses /side')
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 50, 0, 0))
        os.kill(client_process.pid, signal.SIGWINCH)
        drain(.3)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'Usage summary...' in screen_text(rendered), rendered
        os.write(master, b'u')
        menu.wait(timeout=3)
        wait_for_text(client_process, 'Usage · OpenCode'.encode())
        os.write(master, b'\x1b')
        drain(.7)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
        os.kill(client_process.pid, signal.SIGWINCH)
        drain(.3)
        print('PASS narrow Agent menu opens usage summary')
        menu = open_menu([str(repo / 'bin/workbench-agent-usage'), 'menu', client],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, 'Claude Code')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        # Wait for the reopened menu, then for the callback to finish after
        # Escape. Otherwise it may open over the next test's native menu.
        wait_for_text(client_process, 'Usage · Claude Code'.encode())
        assert tmux('show-option', '-gqv', '@workbench-usage-source') == 'claude'
        os.write(master, b'\x1b')
        deadline = time.monotonic() + 10
        while not usage_choice_done.exists():
            if time.monotonic() >= deadline:
                raise AssertionError('Usage source callback did not close')
            drain(0.05)
        print('PASS usage touch action')
        # Stale callbacks must not type into a replacement shell.
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        tmux('respawn-pane', '-k', '-t', pane, '/bin/sh')
        os.write(master, b's')
        menu.wait(timeout=3)
        drain(0.4)
        assert '/side' not in tmux('capture-pane', '-p', '-t', pane)
        wait_for_pane_command(pane, ('sh', 'bash', 'dash'))
        wait_for_agent_actions(pane, '0')
        menu = open_menu([str(repo / 'bin/workbench-menu'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu, b'No foreground agent')
        # Earlier asynchronous redraws may still contain the replaced agent's
        # commands. The stale callback above verifies that /side cannot reach
        # the shell; wait for this menu's own no-agent state here.
        assert b'No foreground agent' in screen_text(rendered)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        drain()
        missing = subprocess.run([core, 'status-menu', 'agent', '--pane', '%999999', '--client', client,
                                  '--guard', 'stale', '--action', '/side'], env=env, capture_output=True)
        assert missing.returncode == 0, missing.stderr
        assert tmux('display-message', '-p', '-t', pane, '#{pane_in_mode}') == '0'
        print('PASS stale/missing targets rejected without an output pager')

        # Agent selection opens one panel; Enter launches the default selection.
        drain(0.3)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        # A preceding menu can leave a pending redraw containing "close".
        # Wait for this menu's own action before sending its first shortcut.
        wait_for_text(menu, b'Launch agent...')
        for key, expected in [(b'a', b'choose agent'), (b'x', b'daily-single')]:
            started = time.monotonic()
            os.write(master, key)
            wait_for_text(client_process, expected)
            if key == b'x':
                print('Cold agent selection redraw milliseconds:', round((time.monotonic() - started) * 1000), flush=True)
            drain(0.15)
        assert not preview_calls.exists(), 'Initial agent selection started the configuration resolver'
        latencies = []
        for key, expected in [(b'h', b'hard-single'), (b't', b'hard-lead'), (b'b', b'batch-lead'), (b's', b'batch-single'), (b'd', b'daily-single')]:
            started = time.monotonic()
            os.write(master, key)
            wait_for_text(client_process, expected)
            latencies.append(round((time.monotonic() - started) * 1000))
            drain(0.05)
        assert not preview_calls.exists(), 'Switching choices started the configuration resolver'
        personal_settings = Path(env['XDG_CONFIG_HOME']) / 'agent-team/config.json'
        personal_settings.parent.mkdir(parents=True, exist_ok=True)
        personal_settings.write_text(json.dumps({'codex': {'scenes': {'hard': {
            'lead_model': 'personal-lead', 'lead_effort': 'max'}}}}))
        for key, expected in [(b'h', b'hard-single'), (b't', b'personal-lead'), (b'd', b'daily-lead'), (b's', b'daily-single')]:
            os.write(master, key)
            wait_for_text(client_process, expected)
            drain(0.05)
        personal_settings.unlink()
        assert not preview_calls.exists(), 'Reading changed settings started the resolver'

        print('PASS cold agent selection and switches avoid resolver processes; redraw milliseconds:', latencies)
        before = tmux('list-windows', '-F', '#{window_id}').splitlines()
        os.write(master, b'\r')
        for _ in range(40):
            drain(0.05)
            after = tmux('list-windows', '-F', '#{window_id}').splitlines()
            if len(after) > len(before): break
        assert len(after) == len(before) + 1
        launched = next(w for w in after if w not in before)
        wait_for_pane_command(launched, 'node')
        assert tmux('display-message', '-p', '-t', launched, '#{pane_current_path}') == tmux('display-message', '-p', '-t', pane, '#{pane_current_path}')
        tmux('kill-window', '-t', launched)
        drain()
        print('PASS one-panel default Enter launch preserves source cwd')
        config_dir = Path(env['XDG_CONFIG_HOME']) / 'tmux-agent-workbench'
        config_dir.mkdir(parents=True, exist_ok=True)
        reject_scene.touch()
        menu = open_menu([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                          '--view', 'configure', '--scene', 'hard'], env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'hard-single')
        before = tmux('list-windows', '-F', '#{window_id}')
        os.write(master, b'\r')
        menu.wait(timeout=3)
        rendered = wait_for_text(client_process, b'Configured model is unavailable')
        assert b'hard-single' in screen_text(rendered)
        os.write(master, b'\x7f')
        wait_for_text(client_process, b'choose agent')
        os.write(master, b'\x1b')
        assert tmux('list-windows', '-F', '#{window_id}') == before
        reject_scene.unlink()
        drain(0.7)
        print('PASS unavailable scene reports an error without opening a window')

        # On short clients the error must be in the footer of every page,
        # rather than disappearing before the page containing it is opened.
        # PTY reads may split after the error row; wait for the Launch hint too.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 15, 80, 0, 0))
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        reject_scene.touch()
        menu = open_menu([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                          '--view', 'configure', '--scene', 'hard'], env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'(enter)')
        os.write(master, b'\r')
        menu.wait(timeout=3)
        rendered = wait_for_text(client_process, b'Configured model is unavailable', b'(enter)')
        for key in (b']', b'['):
            assert re.search(rb'Configured model is unavailable.*?\(enter\)', screen_text(rendered), re.S), rendered
            drain()
            os.write(master, key)
            rendered = wait_for_text(client_process, b'Configured model is unavailable', b'(enter)')
        assert b'(enter)' in screen_text(rendered)
        # A new selection clears the old launch failure and stays on this panel.
        # Check the current screen; the PTY may still contain older redraws
        # with the error even after the newly selected scene has replaced it.
        drain()
        os.write(master, b'd')
        wait_for_text(client_process, '● Daily development'.encode(), b'(enter)',
                      current_screen=True, absent=(b'Configured model is unavailable',))
        os.write(master, b'\x7f')
        wait_for_text(client_process, b'choose agent')
        os.write(master, b'\x1b')
        reject_scene.unlink()
        drain(0.7)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        print('PASS short launch errors stay above Launch across pages and clear on a new choice')

        # Record launches at the public agent-team boundary, without paid API
        # calls. The pane backend creates its own window; no launcher survives.
        trace = root / 'team-launch.txt'
        team_cli = shim / 'agent-team'
        team_cli.write_text('#!/bin/sh\nset -eu\n' + preview_handler + 'case " $* " in *" --check "*) exit 0 ;; esac\n' +
            'printf "%s\\n" "$PWD" "$@" > ' + shlex.quote(str(trace)) + '\n' +
            'case " $* " in *" --tmux "*)\n' +
            'session=$(tmux display-message -p -t "$TMUX_PANE" "#{session_id}")\n' +
            'tmux new-window -t "$session" -n test-team -c "$PWD" ' + shlex.quote(str(shim / 'codex')) + '\n' +
            ';; *) exec ' + shlex.quote(str(shim / 'codex')) + ' ;; esac\n')
        team_cli.chmod(0o755)
        for scene, panes, expected in [('daily', False, ['start', 'codex', '--scene', 'daily', '--team']), ('batch', True, ['start', 'codex', '--scene', 'batch', '--team', '--tmux'])]:
            menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_for_text(menu, b'Launch agent...')
            for key, heading in [(b'a', b'choose agent'), (b'x', b'daily-single')]:
                os.write(master, key)
                wait_for_text(client_process, heading)
                drain(0.15)
            if scene == 'batch':
                os.write(master, b'b')
                wait_for_text(client_process, b'batch-single')
            os.write(master, b't')
            rendered = wait_for_text(client_process, (scene + '-lead').encode())
            assert b'Worker: worker-model' in screen_text(rendered)
            assert '临时调整'.encode() not in screen_text(rendered)
            if panes:
                os.write(master, b'm')
                wait_for_text(client_process, b'close')
                drain(0.15)
            before = tmux('list-windows', '-F', '#{window_id}').splitlines()
            os.write(master, b'\r')
            for _ in range(60):
                drain(0.05)
                after = tmux('list-windows', '-F', '#{window_id}').splitlines()
                if len(after) > len(before) and trace.exists(): break
            assert len(after) == len(before) + 1, (before, after)
            recorded = trace.read_text().splitlines()
            source_cwd = tmux('display-message', '-p', '-t', pane, '#{pane_current_path}')
            assert Path(recorded[0]).samefile(source_cwd), (recorded[0], source_cwd)
            assert recorded[1:] == expected, recorded
            tmux('switch-client', '-c', client, '-t', original_window)
            tmux('kill-window', '-t', next(w for w in after if w not in before))
            trace.unlink()
            drain()
        print('PASS daily native Team and batch tmux Team launch exactly one window')

        for tool in ('claude', 'opencode'):
            for preset, key in (('single', b's'), ('team', b't')):
                menu = open_menu([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                                  '--view', 'configure', '--tool', tool], env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                rendered = wait_for_text(menu)
                assert b'Budget' not in screen_text(rendered) and b'launch.toml' not in screen_text(rendered)
                if preset == 'team':
                    os.write(master, key)
                    wait_for_text(client_process, b'Native subagents')
                before = tmux('list-windows', '-F', '#{window_id}').splitlines()
                os.write(master, b'\r')
                for _ in range(60):
                    drain(0.05)
                    after = tmux('list-windows', '-F', '#{window_id}').splitlines()
                    if len(after) > len(before) and trace.exists(): break
                assert len(after) == len(before) + 1, (tool, preset, before, after)
                assert trace.read_text().splitlines()[1:] == ['start', tool] + (['--team'] if preset == 'team' else [])
                tmux('switch-client', '-c', client, '-t', original_window)
                tmux('kill-window', '-t', next(w for w in after if w not in before))
                trace.unlink()
                drain()
        print('PASS Claude and OpenCode expose only Single/Team and launch with tool defaults')
        menu = open_menu([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                          '--view', 'configure', '--language', 'zh'], env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu, '日常开发'.encode(), '模式'.encode(), '启动'.encode(),
                                 absent=(b'Daily development',))
        assert '模式'.encode() in screen_text(rendered) and '启动'.encode() in screen_text(rendered)
        assert b'Daily development' not in '\n'.join(screen_rows(rendered)).encode()
        os.write(master, b't')
        rendered = wait_for_text(client_process, '主代理'.encode())
        assert '协作方式'.encode() in screen_text(rendered) and '原生子代理'.encode() in screen_text(rendered)
        os.write(master, b'\x7f')
        wait_for_text(client_process, '选择 agent'.encode())
        os.write(master, b'q')
        drain(0.7)
        print('PASS Chinese launch panel retains language across callbacks')


        tmux('respawn-pane', '-k', '-t', pane, str(shim / 'codex'))
        wait_for_pane_command(pane, 'node')
        wait_for_pane_text('interactive-ready', pane)
        tmux('select-pane', '-t', pane, '-T', 'Action Required')
        drain(0.2)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu, b'Return to the agent prompt')
        assert b'/side' not in screen_text(rendered) and b'Return to the agent prompt' in screen_text(rendered)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        # Let the terminal's Escape parser settle before opening another menu.
        drain(0.7)
        tmux('select-pane', '-t', pane, '-T', 'Codex idle')
        tmux('copy-mode', '-t', pane)
        drain()
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'/side' not in screen_text(rendered)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        tmux('send-keys', '-t', pane, '-X', 'cancel')
        print('PASS approval and copy mode suppress agent input')

        window = tmux('display-message', '-p', '-t', pane, '#{window_id}')
        worker = tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', pane, str(shim / 'codex'))
        wait_for_agent_actions(worker, '1')
        shells = [tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', pane, '/bin/sh') for _ in range(2)]
        for shell in shells: tmux('select-pane', '-t', shell)
        for shell in shells: wait_for_agent_actions(shell, '0')
        tmux('set-option', '-wu', '-t', window, '@workbench_last_agent_pane')
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, shells[-1], '--focus-agent'],
                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'Choose agent')
        os.write(master, b'1')
        wait_for_text(client_process, b'/side')
        assert tmux('display-message', '-p', '#{pane_id}') == pane
        os.write(master, b'\x1b')
        drain(0.7)
        for shell in shells: tmux('kill-pane', '-t', shell)
        shell = tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', pane, '/bin/sh')
        tmux('select-pane', '-t', shell)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, shell, '--focus-agent'],
                         env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'/side')
        assert tmux('display-message', '-p', '#{pane_id}') == pane
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        drain(0.7)
        tmux('kill-pane', '-t', shell)
        print('PASS multi-agent chooser and remembered-agent status routing')

        for member in (pane, worker): tmux('set-option', '-p', '-t', member, '@agent_team', 'menu-test')
        state_dir = Path(env['XDG_STATE_HOME']) / 'agent-team/menu-test'
        state_dir.mkdir(parents=True)
        (state_dir / 'state.json').write_text(json.dumps({'id': 'menu-test', 'window': window, 'socket': socket,
            'members': {'lead': {'pane': pane, 'status': 'running'}, 'worker_a': {'pane': worker, 'status': 'ready'}}}))
        drain()
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu, b'Team members & progress...')
        os.write(master, b't')
        rendered = wait_for_text(client_process, b'worker_a')
        assert b'lead' in screen_text(rendered) and b'ready' in screen_text(rendered)
        drain(0.2)
        # Team layout uses the same sidebar-preserving helper as Alt+4.
        sidebar = tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-h', '-b', '-l', '18', '-t', pane, '/bin/sh')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        tmux('set-option', '-g', '@sidebar_width', '18')
        os.write(master, b'L')
        for _ in range(40):
            drain(0.05)
            if tmux('show-option', '-wqv', '-t', window, '@workbench_layout_preset') == 'main-vertical': break
        drain(0.3)
        assert tmux('display-message', '-p', '-t', sidebar, '#{pane_width}') == '18'
        tmux('kill-pane', '-t', sidebar)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b't')
        wait_for_text(client_process, b'worker_a')
        drain(0.2)
        os.write(master, b'2')
        for _ in range(40):
            drain(0.05)
            if tmux('display-message', '-p', '#{pane_id}') == worker: break
        assert tmux('display-message', '-p', '#{pane_id}') == worker
        tmux('kill-pane', '-t', worker)
        tmux('set-option', '-p', '-u', '-t', pane, '@agent_team')
        drain()
        print('PASS Team member roles, reported progress and pane navigation')

        # Narrow clients retain usable menus and Close rather than rejecting width.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 30, 0, 0))
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        menu = open_menu([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b'\x7f')
        menu.wait(timeout=3)
        drain(0.7)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 10, 30, 0, 0))
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        menu = open_menu([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                                 '--view', 'configure', '--tool', 'codex'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu, b'(enter)', b'(back)', b'(esc)')
        # Disabled cwd/summary rows remain inert instead of dismissing the menu.
        x, y = item_position(rendered, '/tmp/')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        rendered += drain(.15)
        assert menu.poll() is None
        visited = []
        for _ in range(6):
            visible = '\n'.join(screen_rows(rendered))
            assert all(hint in visible for hint in ('(enter)', '(back)', '(esc)')), visible
            assert item_position(rendered, '← Back')[1] == item_position(rendered, 'close')[1]
            visited.append(visible)
            os.write(master, b']')
            # Preserve the current screen: the shared renderer only emits
            # changed cells, so the pinned footer need not be written again.
            rendered += drain(.15)
        assert any('Daily development' in screen for screen in visited)
        assert any('Batch work' in screen for screen in visited)
        os.write(master, b'\x7f')
        wait_for_text(client_process, b'choose agen')
        os.write(master, b'\x1b')
        drain()
        print('PASS Back stays beside Close across submenu pages')
        print('PASS narrow client with paginated actions')
        write_frame(termux, {'type': 'goodbye', 'version': 2})
        termux.wait(timeout=3)
    finally:
        if menu is not None and menu.poll() is None:
            menu.terminate()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if client_process is not None:
            try:
                client_process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                client_process.terminate()
                try:
                    client_process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    client_process.kill()
                    client_process.wait(timeout=3)
        if termux is not None and termux.poll() is None:
            termux.terminate()
            termux.wait(timeout=3)
        os.close(master)
        os.close(slave)
