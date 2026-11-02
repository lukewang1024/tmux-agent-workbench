#!/usr/bin/env python3
"""Model Termux's delayed URL lookup against an actual tmux popup's screen."""
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid

from daemon_fixture import running_daemon
from terminal_screen import screen_rows

core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='wb-tap-') as directory, \
        tempfile.TemporaryDirectory(prefix='r') as runtime, ExitStack() as resources:
    root = Path(directory)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_BIN=core,
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket, XDG_RUNTIME_DIR=runtime)
    for key in ('TMUX', 'TMUX_PANE'):
        env.pop(key, None)
    for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME'):
        env[key] = str(root / key)
    env['HOME'] = str(root / 'home')
    Path(env['HOME']).mkdir()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))
    transcript = bytearray()
    client_process = control = menu = None

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def drain(seconds=.1):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([master], [], [], max(0, end - time.monotonic()))[0]:
                transcript.extend(os.read(master, 65536))

    def rows():
        return screen_rows(transcript, 80, 30)

    def wait_for(test):
        deadline = time.monotonic() + 5
        while not test():
            assert time.monotonic() < deadline, ('timed out', rows())
            drain(.02)

    def frame(message):
        payload = json.dumps(message).encode()
        control.stdin.write(len(payload).to_bytes(4, 'big') + payload)
        control.stdin.flush()

    def word_at(x, y):
        # TerminalBuffer.getWordAtLocation() uses the current screen at callback
        # time, not the screen that was present when the finger touched down.
        line = rows()[y]
        if line[x] == ' ':
            return ''
        return line[line.rfind(' ', 0, x) + 1:line.find(' ', x) if ' ' in line[x:] else len(line)]

    def stop(process):
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)

    try:
        url = 'https://behind.invalid/' + 'x' * 56
        background = ''.join(f'\x1b[{row};1H{url}' for row in range(1, 31))
        command = shlex.join([sys.executable, '-u', '-c',
                              'import sys,time;sys.stdout.write(' + repr(background) + ');sys.stdout.flush();time.sleep(1000)'])
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'tap', '-x', '80', '-y', '30', command)
        tmux('set-option', '-g', 'default-shell', '/bin/sh')
        tmux('set-option', '-g', 'status', 'off')
        tmux('set-option', '-g', 'mouse', 'on')
        tmux('set-option', '-g', 'exit-unattached', 'off')
        tmux('set-option', '-g', 'exit-empty', 'off')
        pane = tmux('display-message', '-p', '#{pane_id}')
        env['TMUX'] = f'{socket},{tmux("display-message", "-p", "#{pid}")},0'
        client_process = subprocess.Popen(['tmux', '-S', socket, 'attach', '-t', 'tap'],
                                          env=env, stdin=slave, stdout=slave, stderr=slave)
        wait_for(lambda: bool(tmux('list-clients', '-F', '#{client_name}')))
        client = tmux('list-clients', '-F', '#{client_name}')
        resources.enter_context(running_daemon(core, env))
        control = subprocess.Popen([core, 'client', 'serve'], env=env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        frame({'type': 'hello', 'version': 2, 'device_id': str(uuid.uuid4()),
               'terminal_id': str(uuid.uuid4()), 'device_label': 'tap-fixture',
               'kind': 'termux', 'capabilities': []})
        length = int.from_bytes(control.stdout.read(4), 'big')
        welcome = json.loads(control.stdout.read(length))
        assert welcome['type'] == 'welcome', welcome
        try:
            subprocess.run([core, 'client', 'attach-pty', '--bind', welcome['attachment_token']],
                           env=dict(env, SSH_TTY=os.ttyname(slave)), stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=1)
        except subprocess.TimeoutExpired:
            pass
        subprocess.run([core, 'ui', 'new'], env=env, check=True, stdout=subprocess.DEVNULL)
        for case in ('action', 'url-choice', 'outside'):
            tmux('set-option', '-gu', '@tap-selected')
            label = 'https://menu.invalid/choice' if case == 'url-choice' else 'Safe action'
            wait_for(lambda: 'https://behind.invalid/' in '\n'.join(rows()))
            menu = subprocess.Popen([core, 'display-menu', '--', '-c', client, '-t', pane,
                                     '-T', 'Delayed tap', label, 'a',
                                     'set-option -g @tap-selected yes'], env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_for(lambda: label in '\n'.join(rows()) and '(esc)' in '\n'.join(rows()))
            if case == 'outside':
                x, y = 1, 1
            else:
                y, line = next((y, line) for y, line in enumerate(rows()) if label in line)
                x, y = line.index(label) + 2, y + 1
            os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
            # GestureAndScaleRecognizer in Termux 0.118.3 calls onSingleTapUp
            # after the 300ms Android double-tap timeout, even after onUp sent
            # this press/release pair to tmux. Its URL handler ignores capture.
            drain(.3)
            word = word_at(x - 1, y - 1)
            assert not re.search(r'https?://', word), (case, 'delayed tap opened URL', word)
            menu.wait(timeout=5)
            assert menu.returncode == 0, menu.stderr.read()
            wait_for(lambda: tmux('show-option', '-gqv', '@tap-selected') ==
                     ('' if case == 'outside' else 'yes'))
            drain(.2)
            assert 'https://behind.invalid/' in '\n'.join(rows()), 'background was not restored'
            print('PASS Termux delayed URL lookup:', case, flush=True)
        frame({'type': 'goodbye', 'version': 2})
        control.wait(timeout=3)
    finally:
        try:
            stop(menu)
            stop(control)
            resources.close()
        finally:
            subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            os.close(master)
            os.close(slave)
            stop(client_process)
