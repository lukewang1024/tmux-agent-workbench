#!/usr/bin/env python3
"""Check real menu mouse dismissal and shortcuts through a tmux client PTY."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid

repo = Path(__file__).resolve().parents[1]
core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='wb-menu-mouse-') as root:
    root = Path(root)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_BIN=core)
    for key in ('TMUX', 'TMUX_PANE'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME'):
        env[key] = str(root / key)
    shim = root / 'bin'
    shim.mkdir()
    ssh = shim / 'ssh-connect'
    ssh.write_text('#!/bin/sh\nprintf "%s\\n" dev-host test-host\n')
    ssh.chmod(0o755)
    env['PATH'] = str(shim) + ':' + str(repo / 'bin') + ':' + env['PATH']
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

    def item_position(output, label="close"):
        # Track the native menu's cursor writes to click the rendered Close row.
        screen = {}
        x = y = 0
        tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|[^\x1b]', output.decode(errors='replace'))
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
                elif command == 'J' and numbers[0] == 2: screen.clear()
            elif token == '\r': x = 0
            elif token == '\n': y += 1
            elif token.isprintable():
                screen[x, y] = token
                x += 1
        for row in range(30):
            text = ''.join(screen.get((col, row), ' ') for col in range(80))
            if label in text:
                return text.index(label) + 2, row + 1
        raise AssertionError(('Menu item position missing', label, screen))

    def wait_for_text(proc, needle=b'close'):
        output = b''
        for _ in range(40):
            output += drain(0.05)
            if needle in output:
                return output
            if proc.poll() is not None:
                raise AssertionError(('menu exited before rendering', proc.returncode, proc.stderr.read()))
        raise AssertionError(('menu did not render', output))

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
    daemon_started = False
    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'audit', '-x', '80', '-y', '30', '/bin/sh')
        tmux('set-option', '-g', 'default-shell', '/bin/sh')
        tmux('set-option', '-g', 'mouse', 'on')
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
        for kind in ('host', 'tmux', 'agent', 'usage', 'metrics'):
            for close in ('outside', 'escape', 'button'):
                args = ([str(repo / 'bin/workbench-agent-usage'), 'menu', client] if kind == 'usage'
                        else [str(repo / 'bin/workbench-host-metrics-menu'), client, pane] if kind == 'metrics'
                        else [str(repo / 'bin/workbench-status-popup'), kind, client, pane])
                menu = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                rendered = wait_for_text(menu)
                if close == 'outside':
                    os.write(master, b'\x1b[<0;1;1M\x1b[<0;1;1m')
                elif close == 'escape':
                    os.write(master, b'\x1b')
                else:
                    x, y = item_position(rendered)
                    os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
                for _ in range(40):
                    drain(0.05)
                    if menu.poll() is not None:
                        break
                assert menu.poll() == 0, (kind, close, menu.poll())
                assert tmux('list-windows', '-F', '#{window_id}').count('\n') == 0
                print('PASS', kind, close)
        # Register this PTY as a Termux client so subsequent touch checks take
        # the same no-hover menu path as a real phone attachment.
        subprocess.run([core, 'daemon', 'ensure'], env=env, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        daemon_started = True
        termux = register_termux()
        # Touch sends a press/release without any preceding hover/motion.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, '/btw')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        drain(0.3)
        assert '/btw' in tmux('capture-pane', '-p', '-t', pane), 'touch did not execute selected action'
        tmux('send-keys', '-t', pane, 'C-u')
        print('PASS touch selects action without hover')
        menu = subprocess.Popen([str(repo / 'bin/workbench-host-metrics-menu'), client, pane],
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
        # A native Agent action still prefills the source pane without Enter.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b's')
        menu.wait(timeout=3)
        drain(0.3)
        assert '/side' in tmux('capture-pane', '-p', '-t', pane), tmux('capture-pane', '-p', '-t', pane)
        print('PASS agent action targets source pane')
        # Use the desktop mode for the status mouse binding; Termux touch mode
        # is exercised below through the same native menus opened directly.
        write_frame(termux, {'type': 'goodbye', 'version': 2})
        termux.wait(timeout=3)
        termux = None
        mode = subprocess.check_output([str(repo / 'bin/workbench-menu-mouse-mode'), client],
                                       env=env, text=True).strip()
        assert mode == 'mouse', ('detached Termux client retained touch mode', mode)
        # Exercise the real status binding block: opening press/release must not
        # activate the first row, then mouse motion and click select one action.
        import shlex
        source = (repo / 'workbench.tmux').read_text()
        bindings = source[source.index('tmux bind-key -T root MouseDown1Status if-shell'):]
        bindings = bindings[:bindings.index('tmux bind-key -T root MouseDown3Status')]
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
        tmux('kill-window', '-t', next(window for window in after if window not in before))
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
        tmux('kill-window', '-t', other)
        tmux('set-option', '-g', 'status', 'off')
        drain(0.3)
        print('PASS window tab touch leaves no output pager')
        print('PASS status opening release and subsequent mouse action')
        termux = register_termux()
        menu = subprocess.Popen([str(repo / 'bin/workbench-agent-usage'), 'menu', client],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, 'Claude Code')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        drain(0.5)
        assert tmux('show-option', '-gqv', '@workbench-usage-source') == 'claude'
        # The provider switch reopens Usage; wait for it to settle, then dismiss.
        os.write(master, b'\x1b')
        drain(0.6)
        print('PASS usage touch action')
        # Narrow clients retain usable menus and Close rather than rejecting width.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 30, 0, 0))
        import signal
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        print('PASS narrow client with paginated actions')
        write_frame(termux, {'type': 'goodbye', 'version': 2})
        termux.wait(timeout=3)
    finally:
        if menu is not None and menu.poll() is None:
            menu.terminate()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if client_process is not None:
            client_process.wait(timeout=3)
        if termux is not None and termux.poll() is None:
            termux.terminate()
            termux.wait(timeout=3)
        if daemon_started:
            subprocess.run([core, 'daemon', 'stop'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(master)
        os.close(slave)
