#!/usr/bin/env python3
"""A new client must not restart a compatible live daemon or its sidebar."""
import json
import os
from pathlib import Path
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon

binary = Path(sys.argv[1]).resolve()
old_binary = Path(os.environ.get('WORKBENCH_TEST_ROLLING_DAEMON', str(binary))).resolve()
with tempfile.TemporaryDirectory(prefix='wb-rolling-', dir='/tmp') as directory:
    root = Path(directory)
    socket = str(root / 'tmux.sock')
    client_binary = root / 'new-client'
    shutil.copy2(binary, client_binary)
    env = dict(os.environ, HOME=str(root), TERM='xterm-256color',
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket, XDG_RUNTIME_DIR=str(root))
    for name in ('TMUX', 'TMUX_PANE', 'AGENT_TEAM_DIR', 'WORKBENCH_AGENT'):
        env.pop(name, None)
    for name in ('XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME'):
        env[name] = str(root / name)

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def status():
        return json.loads(subprocess.check_output([str(client_binary), 'daemon', 'status'], env=env, text=True))

    def send(process, message):
        payload = json.dumps(message).encode()
        process.stdin.write(struct.pack('>I', len(payload)) + payload)
        process.stdin.flush()

    try:
        pane = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'test', '-x', '120', '-y', '40',
                    '-P', '-F', '#{pane_id}', '/bin/sh')
        env['TMUX'] = socket + ',' + tmux('display-message', '-p', '#{pid}') + ',0'
        with running_daemon(str(old_binary), env) as daemon:
            sidebar = tmux('split-window', '-d', '-h', '-t', pane, '-P', '-F', '#{pane_id}',
                           str(old_binary) + ' sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            sidebar_pid = tmux('display-message', '-p', '-t', sidebar, '#{pane_pid}')
            deadline = time.monotonic() + 10
            while not tmux('show-option', '-pqv', '-t', sidebar, '@workbench_sidebar_runtime').startswith(sidebar_pid + ':'):
                assert time.monotonic() < deadline, 'sidebar did not register its runtime identity'
                time.sleep(.05)
            before = status()['pid']
            assert before == daemon.pid
            process = subprocess.Popen([str(client_binary), 'client', 'serve'], env=env,
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                send(process, dict(type='hello', version=2, device_id=str(uuid.uuid4()),
                                   terminal_id=str(uuid.uuid4()), device_label='rolling-test',
                                   kind='termux', capabilities=[]))
                assert select.select([process.stdout], [], [], 10)[0], 'client did not register'
                header = process.stdout.read(4)
                assert len(header) == 4, f'client closed before welcome: {process.poll()}'
                reply = json.loads(process.stdout.read(struct.unpack('>I', header)[0]))
                assert reply['type'] == 'welcome', reply
                assert daemon.poll() is None and status()['pid'] == before
                assert tmux('display-message', '-p', '-t', sidebar, '#{pane_pid}') == sidebar_pid
                send(process, dict(type='goodbye', version=2))
                _, error = process.communicate(timeout=8)
                assert process.returncode == 0, error.decode()
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
            assert daemon.poll() is None and status()['pid'] == before
            print('PASS implicit client preserves the compatible daemon and sidebar')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, capture_output=True, timeout=5)
