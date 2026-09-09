#!/usr/bin/env python3
"""Check real menu mouse dismissal and shortcuts through a tmux client PTY."""
import fcntl
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
    env['PATH'] = str(shim) + ':' + env['PATH']
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

    def close_position(output):
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
            if 'close' in text:
                return text.index('close') + 2, row + 1
        raise AssertionError(('Close position missing', screen))

    def wait_for_text(proc, needle=b'close'):
        output = b''
        for _ in range(40):
            output += drain(0.05)
            if needle in output:
                return output
            if proc.poll() is not None:
                raise AssertionError(('menu exited before rendering', proc.returncode, proc.stderr.read()))
        raise AssertionError(('menu did not render', output))

    client_process = None
    menu = None
    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'audit', '-x', '80', '-y', '30')
        tmux('set-option', '-g', 'mouse', 'on')
        tmux('set-option', '-g', 'status', 'off')
        tmux('set-option', '-g', '@workbench-usage-source', 'opencode')
        server_pid = tmux('display-message', '-p', '#{pid}')
        env['TMUX'] = f'{socket},{server_pid},0'
        pane = tmux('display-message', '-p', '#{pane_id}')
        client_process = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'audit'],
                                          stdin=slave, stdout=slave, stderr=slave, env=env)
        for _ in range(40):
            client = tmux('list-clients', '-F', '#{client_name}')
            if client:
                break
            drain(0.05)
        drain()
        for kind in ('host', 'tmux', 'agent', 'usage'):
            for close in ('outside', 'escape', 'button'):
                args = ([str(repo / 'bin/workbench-agent-usage'), 'menu', client] if kind == 'usage'
                        else [str(repo / 'bin/workbench-status-popup'), kind, client, pane])
                menu = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                rendered = wait_for_text(menu)
                # Releasing the click that opened a menu must not dismiss it.
                os.write(master, b'\x1b[<0;1;1m')
                drain(0.1)
                assert menu.poll() is None, (kind, 'opening release dismissed menu')
                if close == 'outside':
                    os.write(master, b'\x1b[<0;1;1M\x1b[<0;1;1m')
                elif close == 'escape':
                    os.write(master, b'\x1b')
                else:
                    x, y = close_position(rendered)
                    os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
                for _ in range(40):
                    drain(0.05)
                    if menu.poll() is not None:
                        break
                assert menu.poll() == 0, (kind, close, menu.poll())
                assert tmux('list-windows', '-F', '#{window_id}').count('\n') == 0
                print('PASS', kind, close)
        # A native Agent action still prefills the source pane without Enter.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b's')
        menu.wait(timeout=3)
        drain(0.3)
        assert '/side' in tmux('capture-pane', '-p', '-t', pane)
        print('PASS agent action targets source pane')
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
    finally:
        if menu is not None and menu.poll() is None:
            menu.terminate()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if client_process is not None:
            client_process.wait(timeout=3)
        os.close(master)
        os.close(slave)
