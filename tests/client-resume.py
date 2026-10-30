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
    env['XDG_RUNTIME_DIR'] = str(root)

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def client_value(client, fmt):
        rows = tmux('list-clients', '-F', '#{client_name}\x1f' + fmt)
        return next(value for name, value in (row.split('\x1f', 1) for row in rows.splitlines()) if name == client)

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

    def heartbeat(channel, activity_ms=1):
        write_frame(channel, dict(type='heartbeat', version=2, activity_unix_ms=activity_ms))
        while True:
            reply = read_frame(channel)
            if reply['type'] == 'heartbeat_ack':
                return

    def registered(client):
        snapshot = json.loads(subprocess.check_output([core, 'snapshot', '--json'], env=env, text=True))
        return next((item for item in snapshot['clients'] if item['attachment'] == client), None)

    device = str(uuid.uuid4())
    terminal_a, terminal_b = str(uuid.uuid4()), str(uuid.uuid4())

    def key(terminal):
        return '@workbench-resume-' + hashlib.sha256((device + '\0' + terminal).encode()).hexdigest()

    def connect(terminal=None, session=None, exclude_client=None):
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
        client = wait_for(lambda: next((name for name in tmux('list-clients', '-f', '#{session_attached}', '-F', '#{client_name}').splitlines()
                                      if name != exclude_client), None), 'client did not attach')
        observed_key = '@workbench-attachment-' + hashlib.sha256(client.encode()).hexdigest()
        wait_for(lambda: json.loads(tmux('show-options', '-gqv', observed_key) or '{}').get('pane_id') == client_value(client, '#{pane_id}'),
                 'live attachment target was not observed')
        return pid, master, client, channel

    def disconnect(connection, terminal=None):
        pid, master, client, channel = connection
        expected = client_value(client, '#{pane_id}')
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
            assert client_value(connection[2], '#{session_name} #{pane_id}') == 'alpha ' + alpha, (client_value(connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', key(terminal_a)))
            disconnect(connection, terminal_a)
            print('PASS reconnect restores its own terminal, not sibling terminal history')
            connection = connect(terminal_a, 'beta')
            assert client_value(connection[2], '#{pane_id}') == beta, (client_value(connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', '@workbench-resume-last'))
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
            assert client_value(connection[2], '#{pane_id}') == beta, (client_value(connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', '@workbench-resume-last'))
            disconnect(connection)
            print('PASS plain SSH uses shared fallback without claiming a terminal identity')
        with running_daemon(core, env):
            connection = connect(terminal_a)
            assert client_value(connection[2], '#{pane_id}') == beta, (client_value(connection[2], '#{session_name} #{pane_id}'), tmux('show-options', '-gqv', '@workbench-resume-last'))
            disconnect(connection, terminal_a)
            print('PASS terminal history survives daemon restart')
        # Keep both SSH-side processes alive across a daemon restart. Their
        # next heartbeat must restore the exact terminal and its attachment.
        with running_daemon(core, env):
            connection = connect(terminal_a, 'alpha')
            heartbeat(connection[3], 1)  # a far-behind client clock
            entry = wait_for(lambda: registered(connection[2]), 'client was not registered')
            assert abs(entry['activity_unix_ms'] - int(time.time() * 1000)) < 10000, entry
        with running_daemon(core, env):
            assert tmux('list-clients', '-F', '#{client_name}') == connection[2]
            heartbeat(connection[3], 2**63)  # a far-ahead client clock
            entry = wait_for(lambda: registered(connection[2]), 'client did not recover after daemon restart')
            assert abs(entry['activity_unix_ms'] - int(time.time() * 1000)) < 10000, entry
            assert tmux('list-clients', '-F', '#{client_name}') == connection[2]
            fields = tmux('display-message', '-p', '-t', 'beta',
                          '#{session_id}\t#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{pane_id}\t#{pane_index}').split('\t')
            target = dict(session_id=fields[0], session_name=fields[1], window_id=fields[2],
                          window_index=int(fields[3]), window_name=fields[4], pane_id=fields[5], pane_index=int(fields[6]))
            write_frame(connection[3], dict(type='focus_target', version=2, event_id='recovered-click', target=target))
            wait_for(lambda: client_value(connection[2], '#{session_name} #{pane_id}') == 'beta ' + beta,
                     'notification click did not focus the recovered attachment')
            disconnect(connection, terminal_a)
            print('PASS live attachment and notification focus recover after daemon restart despite client clock skew')
        # A restart between registration and PTY binding must restore the
        # still-live single-use token before attach-pty retries its bind.
        with running_daemon(core, env):
            channel = subprocess.Popen([core, 'client', 'serve'], env=env,
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            write_frame(channel, dict(type='hello', version=2, device_id=device,
                                      terminal_id=terminal_a, device_label='test', kind='termux', capabilities=[]))
            welcome = read_frame(channel)
        with running_daemon(core, env):
            pid, master = pty.fork()
            if pid == 0:
                os.execvpe(core, [core, 'client', 'attach-pty', '--bind', welcome['attachment_token'], '--session', 'alpha'],
                           dict(env, SSH_TTY=os.ttyname(0)))
            client = wait_for(lambda: tmux('list-clients', '-f', '#{session_attached}', '-F', '#{client_name}'),
                              'client did not bind after daemon restart')
            observed_key = '@workbench-attachment-' + hashlib.sha256(client.encode()).hexdigest()
            wait_for(lambda: json.loads(tmux('show-options', '-gqv', observed_key) or '{}').get('pane_id') == client_value(client, '#{pane_id}'),
                     'recovered binding was not observed')
            disconnect((pid, master, client, channel), terminal_a)
            print('PASS registration token survives restart before PTY binding')
        # The earlier control process is deliberately kept alive after its
        # PTY was replaced. It cannot reclaim the newer terminal on recovery.
        with running_daemon(core, env):
            old = connect(terminal_a, 'alpha')
            new = connect(terminal_a, 'beta', exclude_client=old[2])
            os.waitpid(old[0], 0)
            os.close(old[1])
        with running_daemon(core, env):
            heartbeat(old[3])
            heartbeat(new[3])
            wait_for(lambda: registered(new[2]), 'new terminal did not recover')
            assert tmux('list-clients', '-F', '#{client_name}') == new[2]
            write_frame(old[3], dict(type='goodbye', version=2))
            _, error = old[3].communicate(timeout=5)
            assert error == b'', error
            disconnect(new, terminal_a)
            print('PASS superseded control cannot reclaim or detach the newer terminal')
        before = tmux('list-sessions', '-F', '#{session_id}')
        missing = subprocess.run([core, 'client', 'attach', '--session', 'missing-session'], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert missing.returncode != 0
        assert tmux('list-sessions', '-F', '#{session_id}') == before
        print('PASS missing explicit target fails without creating or attaching a different session')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
