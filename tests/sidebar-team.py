#!/usr/bin/env python3
"""Exercise actual team identity, expansion and navigation on an isolated tmux."""
from contextlib import ExitStack
import fcntl
import json
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

with tempfile.TemporaryDirectory(prefix='wb-team-sidebar-') as root, ExitStack() as processes:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               WORKBENCH_FIXTURE_SECONDS='120')
    for key in ('TMUX', 'TMUX_PANE', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = root + '/' + key
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
    client = None

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).rstrip('\n')

    def drain(seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([master], [], [], max(0, end - time.monotonic()))
            if ready:
                os.read(master, 65536)

    def screen():
        return tmux('capture-pane', '-p', '-t', sidebar).splitlines()

    def wait_for(predicate, label):
        deadline = time.monotonic() + 12
        while not predicate():
            if time.monotonic() >= deadline:
                raise AssertionError((label, screen()))
            drain(.05)

    def click(column, row):
        os.write(master, f'\x1b[<0;{column+1};{row+1}M\x1b[<0;{column+1};{row+1}m'.encode())

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'a-team', '-n', 'Team Task',
             '-x', '160', '-y', '40', shlex.quote(fixture))
        lead = tmux('display-message', '-p', '-t', 'a-team', '#{pane_id}')
        window = tmux('display-message', '-p', '-t', lead, '#{window_id}')
        worker = tmux('split-window', '-d', '-t', lead, '-P', '-F', '#{pane_id}', shlex.quote(fixture))
        for pane in (lead, worker):
            tmux('set-option', '-p', '-t', pane, '@agent_team', 'team-test')
        team_dir = Path(env['XDG_STATE_HOME']) / 'agent-team/team-test'
        team_dir.mkdir(parents=True)
        state = {'id': 'team-test', 'socket': socket, 'window': window,
                 'members': {'lead': {'pane': lead}, 'worker_a': {'pane': worker, 'brief': '目标：核对注册信息；只读'}}}
        state_file = team_dir / 'state.json'
        state_file.write_text(json.dumps(state))
        tmux('new-session', '-d', '-s', 'b-destination', '-n', 'Destination', '-x', '160', '-y', '40')
        tmux('new-session', '-d', '-s', 'z-source', '-n', 'Source', '-x', '160', '-y', '40',
             shlex.quote(fixture))
        for option in ('mouse', 'focus-events'):
            tmux('set-option', '-g', option, 'on')
        tmux('set-option', '-g', 'status', 'off')
        processes.enter_context(running_daemon(core, env))
        sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                       '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        client = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'z-source'],
                                  env=env, stdin=slave, stdout=slave, stderr=slave,
                                  start_new_session=True)
        wait_for(lambda: any('▸ Team Task' in line for line in screen()), 'team not collapsed')
        assert not any('worker_a' in line for line in screen()), screen()
        assert any('1 worker' in line for line in screen()), screen()
        row = next(i for i, line in enumerate(screen()) if '▸ Team Task' in line)
        click(1, row)
        wait_for(lambda: any('worker_a' in line for line in screen()), 'arrow did not expand')
        assert sum('worker_a' in line for line in screen()) == 1, screen()
        assert any('核对注册信息' in line for line in screen()), screen()
        state['members']['worker_a']['cursor'] = 1
        state['messages'] = [{'id': 1, 'from': 'lead', 'to': 'worker_a', 'type': 'send',
                              'text': '新任务：检查 Remote 发布情况。请报告。'}]
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        wait_for(lambda: any('检查 Remote 发布情况' in line for line in screen()), 'task label did not update')
        tmux('send-keys', '-t', sidebar, 'Space')
        wait_for(lambda: not any('worker_a' in line for line in screen()), 'space did not collapse')
        tmux('send-keys', '-t', sidebar, 'Right')
        wait_for(lambda: any('worker_a' in line for line in screen()), 'right did not expand')
        tmux('send-keys', '-t', sidebar, 'h')
        wait_for(lambda: not any('worker_a' in line for line in screen()), 'h did not collapse')
        tmux('send-keys', '-t', sidebar, 'l')
        wait_for(lambda: any('worker_a' in line for line in screen()), 'l did not expand')
        # Moving the task window carries both panes; regrouping keeps the
        # worker nested under the same lead after the snapshot catches up.
        tmux('move-window', '-s', window, '-t', 'b-destination:')
        def team_in_destination():
            rows = screen()
            return any(line.strip() == 'b-destination' and index + 1 < len(rows)
                       and '▾ Team Task' in rows[index + 1]
                       for index, line in enumerate(rows))

        wait_for(team_in_destination, 'team did not move together')
        assert tmux('display-message', '-p', '-t', worker, '#{window_id}') == window
        assert tmux('display-message', '-p', '-t', worker, '#{session_name}') == 'b-destination'
        # Check identity while the sidebar is still visible. A hidden sidebar
        # intentionally throttles rendering and its captured screen may be stale.
        state['socket'] = '/tmp/not-the-same-socket'
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        wait_for(lambda: not any('▾ Team Task' in line or '▸ Team Task' in line for line in screen())
                 and sum('Team Task' in line for line in screen()) >= 2, 'invalid team was grouped')
        state['socket'] = socket
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        wait_for(lambda: team_in_destination() and any('worker_a' in line for line in screen()),
                 'restored team was not grouped')
        # A stopped worker remains in agent-team state and in the daemon's
        # retained snapshots, but must not reappear as an independent agent.
        stopped = tmux('split-window', '-d', '-t', lead, '-P', '-F', '#{pane_id}', shlex.quote(fixture))
        tmux('set-option', '-p', '-t', stopped, '@agent_team', 'team-test')
        state['members']['worker_b'] = {'pane': stopped, 'brief': 'Stopped worker unique task'}
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        wait_for(lambda: any('worker_b' in line for line in screen()), 'second worker was not grouped')
        tmux('kill-pane', '-t', stopped)
        state['members']['worker_b']['status'] = 'stopped'
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        def stopped_snapshot():
            result = subprocess.check_output([core, 'snapshot', '--json'], env=env, text=True)
            return any(agent['target']['pane_id'] == stopped and agent['exited']
                       for agent in json.loads(result)['agents'])
        wait_for(stopped_snapshot, 'daemon did not retain the stopped worker')
        wait_for(lambda: not any('worker_b' in line or 'Stopped worker unique task' in line for line in screen())
                 and sum('Team Task' in line for line in screen()) == 1,
                 'stopped worker escaped to top level')
        row = next(i for i, line in enumerate(screen()) if 'worker_a' in line)
        click(5, row)
        wait_for(lambda: tmux('list-clients', '-F', '#{pane_id}') == worker, 'worker click did not focus')
        # The daemon retains unseen stopped-worker snapshots. Closing the lead
        # and all team panes must not promote those snapshots to top-level rows.
        tmux('switch-client', '-t', 'z-source')
        state['closed'] = True
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        tmux('kill-pane', '-t', worker)
        tmux('kill-pane', '-t', lead)
        wait_for(stopped_snapshot, 'stopped worker snapshot disappeared before closure check')
        # Invalid provenance must expose the retained snapshot again, proving
        # it still exists and that the subsequent suppression uses team state.
        state['socket'] = '/tmp/not-the-same-socket'
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        wait_for(lambda: sum('Team Task' in line for line in screen()) >= 2,
                 'retained worker snapshot was not observable')
        state['socket'] = socket
        state_file.write_text(json.dumps(state, ensure_ascii=False))
        wait_for(lambda: sum('Team Task' in line for line in screen()) <= 1,
                 'stopped workers reappeared after lead exit')
        # Reopening the sidebar must recover the same suppression from disk.
        tmux('kill-pane', '-t', sidebar)
        sidebar = tmux('split-window', '-b', '-h', '-l', '36', '-t', 'z-source',
                       '-P', '-F', '#{pane_id}', shlex.quote(core) + ' sidebar')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        wait_for(lambda: any('agents' in line.lower() for line in screen()), 'sidebar did not reopen')
        drain(.5)
        assert sum('Team Task' in line for line in screen()) <= 1, screen()
        assert stopped_snapshot(), 'the check must cover a retained worker snapshot'
        print('PASS: team sidebar navigation, identity, and closed-team worker suppression')
    finally:
        os.close(master)
        os.close(slave)
        if client is not None:
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)
        processes.close()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
