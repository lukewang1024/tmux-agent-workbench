#!/usr/bin/env python3
"""A designated assistant stays pinned when the same thread gets a new window."""
import json
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
import uuid

from daemon_fixture import running_daemon

core = str(Path(sys.argv[1]).resolve())
fixture = str(Path(__file__).resolve().parents[1] / 'target/debug/examples/codex')
with tempfile.TemporaryDirectory(prefix='wb-assistant-pin-', dir='/tmp') as root:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               TMUX_AGENT_WORKBENCH_LANGUAGE='en', WORKBENCH_FIXTURE_SECONDS='120')
    for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = root + '/' + key
    state = Path(env['XDG_STATE_HOME'], 'tmux-agent-workbench')
    state.mkdir(parents=True)
    thread = str(uuid.uuid4())
    (state / 'personal-assistant.json').write_text(json.dumps(
        {'schema_version': 1, 'kind': 'codex', 'thread_id': thread}))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 160, 0, 0))
    client = None

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True, timeout=10).rstrip('\n')
    def call(*args, data=None):
        result = subprocess.run([core, *args], env=env, text=True, capture_output=True,
                                input=json.dumps(data) if data is not None else None, timeout=10)
        assert result.returncode == 0, (args, result.stderr)
        return json.loads(result.stdout)
    def live(pane):
        return next((a for a in call('snapshot', '--json')['agents']
                     if not a['exited'] and a['target']['pane_id'] == pane), None)
    def wait(predicate, label):
        deadline = time.monotonic() + 15
        while not predicate():
            assert time.monotonic() < deadline, (label, screen())
            time.sleep(.05)
    def bind(pane):
        wait(lambda: live(pane), 'agent detection')
        call('hook', 'ingest', 'codex', 'SessionStart', data={
            'session_id': thread, 'agent_pid': live(pane)['process']['pid']})
        wait(lambda: live(pane).get('hook_session_id') == thread, 'thread binding')
    def screen():
        return tmux('capture-pane', '-p', '-t', sidebar).splitlines()
    def drain(seconds=.1):
        data = b''
        end = time.monotonic() + seconds
        while time.monotonic() < end and select.select([master], [], [], max(0, end-time.monotonic()))[0]:
            data += os.read(master, 65536)
        return data

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'a-observer', '-n', 'shell',
             '-x', '160', '-y', '30', 'sleep 120')
        tmux('set-option', '-g', 'mouse', 'on')
        tmux('set-option', '-g', 'focus-events', 'on')
        observer = tmux('display-message', '-p', '-t', 'a-observer', '#{pane_id}')
        observer_window = tmux('display-message', '-p', '-t', observer, '#{window_id}')
        tmux('set-option', '-w', '-t', observer_window, '@agent_focus', '1')
        original = tmux('new-session', '-d', '-s', 'z-assistant', '-n', 'agent',
                        '-P', '-F', '#{pane_id}', shlex.quote(fixture))
        with running_daemon(core, env):
            bind(original)
            sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', observer,
                           '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            # Generic window titles such as "shell" use the tmux session name.
            wait(lambda: len(screen()) > 5 and ' 2' in screen()[0]
                 and 'Personal assistant' in screen()[1] and 'a-observer' in screen()[3],
                 'assistant is pinned ahead of an alphabetically earlier task')
            assert tmux('show-option', '-wqv', '-t', original, '@agent_focus') == ''
            assert tmux('show-option', '-wqv', '-t', original, '@workbench_assistant') == ''
            client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'a-observer'],
                                      env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            tmux('select-pane', '-t', sidebar)
            drain(.5)
            os.write(master, b'\x1b[<35;5;2M')  # Hover the personal-assistant card.
            drain(.2)
            os.write(master, b'p')
            time.sleep(.4)
            assert 'Personal assistant' in screen()[1], screen()
            assert tmux('show-option', '-wqv', '-t', original, '@agent_focus') == ''
            os.write(master, b'\x1b[<2;5;2M\x1b[<2;5;2m')
            output = b''
            deadline = time.monotonic() + 8
            while b'Always pinned' not in output and time.monotonic() < deadline:
                output += drain()
            assert b'Always pinned' in output, output[-1200:]
            os.write(master, b'\x1b')
            drain(.3)
            original_window = tmux('display-message', '-p', '-t', original, '#{window_id}')
            tmux('kill-session', '-t', 'z-assistant')
            resumed = tmux('new-session', '-d', '-s', 'z-resumed', '-n', 'agent',
                           '-P', '-F', '#{pane_id}', shlex.quote(fixture))
            assert tmux('display-message', '-p', '-t', resumed, '#{window_id}') != original_window
            bind(resumed)
            wait(lambda: 'Personal assistant' in screen()[1] and 'z-resumed' in screen()[2],
                 'assistant pinned after session resume into a new window')
            assert tmux('show-option', '-wqv', '-t', resumed, '@agent_focus') == ''
            print('PASS personal assistant remains first and reappears after its thread resumes in a new window')
    finally:
        os.close(master)
        os.close(slave)
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
        if client is not None:
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)
