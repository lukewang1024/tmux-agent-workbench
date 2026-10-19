#!/usr/bin/env python3
"""Real PTY reconnects exercise authenticated per-terminal history and precedence."""
import hashlib
import json
import os
from pathlib import Path
import pty
import struct
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon

core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='workbench-resume-') as root:
    root = Path(root)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
    for name in ('TMUX', 'TMUX_PANE'):
        env.pop(name, None)
    for name in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME'):
        env[name] = str(root / name)

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def wait_for(probe, description):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            result = probe()
            if result:
                return result
            time.sleep(.03)
        raise AssertionError(description)

    def write_frame(proc, value):
        data = json.dumps(value).encode()
        proc.stdin.write(struct.pack('>I', len(data)) + data)
        proc.stdin.flush()

    def read_frame(proc):
        size = proc.stdout.read(4)
        assert len(size) == 4, proc.stderr.read()
        return json.loads(proc.stdout.read(struct.unpack('>I', size)[0]))

    device = str(uuid.uuid4())
    terminal_a, terminal_b = str(uuid.uuid4()), str(uuid.uuid4())

    def key(terminal):
        return '@workbench-resume-' + hashlib.sha256((device + '\0' + terminal).encode()).hexdigest()

    def connect(terminal=None, session=None):
        channel = None
        args = [core, 'client', 'attach']
        if terminal:
            channel = subprocess.Popen([core, 'client', 'serve'], env=env,
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            write_frame(channel, dict(type='hello', version=2, device_id=device,
                                      terminal_id=terminal, device_label='test', kind='termux', capabilities=[]))
            welcome = read_frame(channel)
            args = [core, 'client', 'attach-pty', '--bind', welcome['attachment_token']]
        if session:
            args += ['--session', session]
        pid, master = pty.fork()
        if pid == 0:
            os.execvpe(core, args, dict(env, SSH_TTY=os.ttyname(0)))
        client = wait_for(lambda: tmux('list-clients', '-f', '#{session_attached}', '-F', '#{client_name}'), 'client did not attach')
        return pid, master, client, channel

    def disconnect(connection, terminal=None):
        pid, master, client, channel = connection
        expected = tmux('display-message', '-p', '-c', client, '#{pane_id}')
        tmux('detach-client', '-t', client)
        os.waitpid(pid, 0)
        os.close(master)
        wait_for(lambda: not tmux('list-clients', '-F', '#{client_name}'), 'client did not detach')
        # The daemon's live observer can update terminal history before the
        # asynchronous detach hook has committed the shared fallback. Observe
        # that final result instead of treating client disappearance as a receipt.
        wait_for(lambda: json.loads(tmux('show-options', '-gqv', '@workbench-resume-last') or '{}').get('pane_id') == expected,
                 'detach hook did not save the shared fallback')
        if terminal:
            wait_for(lambda: (json.loads(tmux('show-options', '-gqv', key(terminal)) or '{}').get('pane_id') == expected), 'terminal history not saved')
        if channel:
            write_frame(channel, dict(type='goodbye', version=2))
            channel.communicate(timeout=5)

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'alpha', 'sleep 120')
        tmux('split-window', '-h', '-t', 'alpha', 'sleep 120')
        alpha = tmux('display-message', '-p', '-t', 'alpha', '#{pane_id}')
        tmux('new-session', '-d', '-s', 'beta', 'sleep 120')
        beta = tmux('display-message', '-p', '-t', 'beta', '#{pane_id}')
        with running_daemon(core, env):
            # Some detach events have no target, or run after the pane exited.
            # They must succeed without inventing a target or erasing history.
            for missing_pane, missing_session in [('', ''), ('%999999', '$999999')]:
                before = tmux('show-options', '-gqv', '@workbench-resume-last')
                missing_target = subprocess.run([core, 'client', 'remember', '--attachment', '/dev/pts/missing',
                                                  '--pane', missing_pane, '--session', missing_session],
                                                 env=env, capture_output=True, text=True)
                assert missing_target.returncode == 0, missing_target.stderr
                assert tmux('show-options', '-gqv', '@workbench-resume-last') == before
            print('PASS targetless and closed-target detach are successful no-ops')
            hook = f"run-shell -b \"{core} client remember --attachment '#{{hook_client}}' --pane '#{{hook_pane}}' --session '#{{hook_session}}'\""
            tmux('set-hook', '-g', 'client-detached[920]', hook)
            disconnect(connect(terminal_a, 'alpha'), terminal_a)
            disconnect(connect(terminal_b, 'beta'), terminal_b)
            connection = connect(terminal_a)
            assert tmux('display-message', '-p', '-c', connection[2], '#{session_name} #{pane_id}') == 'alpha ' + alpha, (tmux('display-message', '-p', '-c', connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', key(terminal_a)))
            disconnect(connection, terminal_a)
            print('PASS reconnect restores its own terminal, not sibling terminal history')
            connection = connect(terminal_a, 'beta')
            assert tmux('display-message', '-p', '-c', connection[2], '#{pane_id}') == beta, (tmux('display-message', '-p', '-c', connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', '@workbench-resume-last'))
            disconnect(connection, terminal_a)
            print('PASS explicit session overrides saved terminal target')
            saved = tmux('show-options', '-gqv', '@workbench-resume-last')
            no_target = subprocess.run([core, 'client', 'remember', '--attachment', '/dev/pts/missing',
                                        '--pane', '', '--session', ''], env=env, capture_output=True, text=True)
            assert no_target.returncode == 0, no_target.stderr
            assert tmux('show-options', '-gqv', '@workbench-resume-last') == saved
            assert json.loads(tmux('show-options', '-gqv', key(terminal_a)))['pane_id'] == beta
            print('PASS targetless detach preserves existing shared and terminal histories')
            wait_for(lambda: tmux('show-options', '-gqv', '@workbench-resume-last'), 'shared fallback not saved')
            connection = connect()
            assert tmux('display-message', '-p', '-c', connection[2], '#{pane_id}') == beta, (tmux('display-message', '-p', '-c', connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', '@workbench-resume-last'))
            disconnect(connection)
            print('PASS plain SSH uses shared fallback without claiming a terminal identity')
        with running_daemon(core, env):
            connection = connect(terminal_a)
            assert tmux('display-message', '-p', '-c', connection[2], '#{pane_id}') == beta, (tmux('display-message', '-p', '-c', connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', '@workbench-resume-last'))
            disconnect(connection, terminal_a)
            print('PASS terminal history survives daemon restart')
        before = tmux('list-sessions', '-F', '#{session_id}')
        missing = subprocess.run([core, 'client', 'attach', '--session', 'missing-session'], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert missing.returncode != 0
        assert tmux('list-sessions', '-F', '#{session_id}') == before
        print('PASS missing explicit target fails without creating or attaching a different session')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
