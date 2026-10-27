#!/usr/bin/env python3
"""An installer upgrade must hand off resident sidebars before the next click."""
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

from daemon_fixture import running_daemon

source = Path(sys.argv[1]).resolve()
repo = Path(__file__).resolve().parents[1]
fixture = repo / 'target/debug/examples/codex'

# Short paths keep the two servers in their own runtime on macOS, too.
with tempfile.TemporaryDirectory(prefix='wb-upgrade-', dir='/tmp') as directory, ExitStack() as processes:
    root = Path(directory).resolve()
    env = dict(os.environ, TERM='xterm-256color', WORKBENCH_FIXTURE_SECONDS='120')
    for key in ('TMUX', 'TMUX_PANE', 'TMUX_AGENT_WORKBENCH_TMUX_SOCKET',
                'TMUX_AGENT_WORKBENCH_BIN', 'WORKBENCH_SOURCE_PANE', 'WORKBENCH_POPUP'):
        env.pop(key, None)
    for key, name in (('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'),
                      ('XDG_STATE_HOME', 'state'), ('XDG_CACHE_HOME', 'cache'),
                      ('XDG_RUNTIME_DIR', 'run'), ('HOME', 'home')):
        env[key] = str(root / name)
    core = root / 'data/tmux-agent-workbench/bin/tmux-agent-workbench-core'
    core.parent.mkdir(parents=True)
    shutil.copy2(source, core)
    plugin = root / 'plugin with spaces'
    (plugin / 'target/release').mkdir(parents=True)
    shutil.copy2(repo / 'install', plugin / 'install')
    shutil.copy2(repo / 'Cargo.toml', plugin / 'Cargo.toml')
    shutil.copy2(source, plugin / 'target/release/tmux-agent-workbench')
    for name in ('bin', 'lib'):
        shutil.copytree(repo / name, plugin / name)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
    client = None
    servers = []

    def drain(seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([master], [], [], max(0, end - time.monotonic()))
            if ready:
                os.read(master, 65536)

    def wait_for(predicate, label):
        deadline = time.monotonic() + 10
        while not predicate():
            if time.monotonic() >= deadline:
                raise AssertionError(label)
            drain(.05)

    def run(*args, environment=env):
        return subprocess.check_output(args, env=environment, text=True).rstrip('\n')

    def tmux(server, *args):
        return run('tmux', '-S', server['socket'], *args)

    def snapshot(server):
        return json.loads(run(str(core), 'snapshot', '--json', environment=server['env']))

    def panes(server):
        return {row.split('\t')[0]: row.split('\t')[1:] for row in tmux(
            server, 'list-panes', '-a', '-F',
            '#{pane_id}\t#{pane_pid}\t#{window_id}\t#{pane_active}\t#{pane_current_path}').splitlines()}

    def layout(server):
        return tmux(server, 'list-windows', '-a', '-F',
                    '#{window_id}\t#{window_layout}\t#{window_zoomed_flag}')

    def focus(server):
        return tmux(server, 'list-clients', '-F', '#{client_name}\t#{session_id}\t#{window_id}\t#{pane_id}')

    def agent_identity(server):
        return sorted((a['process']['pid'], a['process']['started_at_ticks'],
                       a['process']['executable'], a['target']['pane_id'])
                      for a in snapshot(server)['agents'] if a.get('process') and not a['exited'])

    def sidebar_ready(server):
        pane = server['sidebar']
        state = tmux(server, 'display-message', '-p', '-t', pane,
                     '#{pane_pid}\t#{@workbench_sidebar_runtime}').split('\t')
        return len(state) == 2 and state[1].startswith(state[0] + ':')

    try:
        for number in (1, 2):
            server = {'socket': str(root / f'{number}.sock')}
            server['env'] = dict(env, TMUX_AGENT_WORKBENCH_TMUX_SOCKET=server['socket'])
            servers.append(server)
            main = tmux(server, '-f', '/dev/null', 'new-session', '-d', '-P', '-F', '#{pane_id}',
                        '-s', 'source', '-n', 'Task source', '-x', '160', '-y', '40',
                        shlex.quote(str(fixture)))
            server['main'] = main
            for option in ('mouse', 'focus-events'):
                tmux(server, 'set-option', '-g', option, 'on')
            tmux(server, 'set-option', '-g', 'status', 'off')
            if number == 1:
                target = tmux(server, 'new-session', '-d', '-P', '-F', '#{pane_id}',
                              '-s', 'target', '-n', 'Task target', '-x', '160', '-y', '40',
                              shlex.quote(str(fixture)))
                # A repurposed pane retaining an old sidebar role is not a UI
                # process and must never be restarted by runtime maintenance.
                support = tmux(server, 'split-window', '-d', '-v', '-t', main, '-P', '-F',
                               '#{pane_id}', 'sleep 120')
                tmux(server, 'set-option', '-p', '-t', support, '@pane_role', 'sidebar')
            processes.enter_context(running_daemon(str(core), server['env']))
            server['sidebar'] = tmux(server, 'split-window', '-d', '-b', '-h', '-f', '-l', '36',
                                     '-t', main, '-P', '-F', '#{pane_id}', str(core), 'sidebar')
            tmux(server, 'set-option', '-p', '-t', server['sidebar'], '@pane_role', 'sidebar')
            wait_for(lambda: sidebar_ready(server), 'sidebar did not register its rendered generation')
            if number == 2:
                tmux(server, 'resize-pane', '-Z', '-t', main)
        primary = servers[0]
        client = subprocess.Popen(['tmux', '-S', primary['socket'], 'attach-session', '-t', 'source'],
                                  env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        wait_for(lambda: focus(primary), 'client did not attach')
        for index, server in enumerate(servers):
            wait_for(lambda: len(agent_identity(server)) == (2 if index == 0 else 1),
                     'agents were not detected before upgrade')
        # Also migrate pre-handoff versions that never wrote a runtime marker.
        tmux(primary, 'set-option', '-pu', '-t', primary['sidebar'], '@workbench_sidebar_runtime')
        before = [{"panes": panes(s), "layout": layout(s), "focus": focus(s),
                   "agents": agent_identity(s)} for s in servers]

        # This is the actual installer, invoked outside tmux. It atomically
        # replaces the resident processes' executable, then discovers both
        # existing servers and runs the same handoff as plugin daemon ensure.
        installer_env = dict(env, TMUX_AGENT_WORKBENCH_INSTALL_SOURCE='1')
        run(str(plugin / 'install'), str(root / 'bin'), '--no-git', environment=installer_env)
        for server, previous in zip(servers, before):
            current = panes(server)
            assert current.keys() == previous['panes'].keys(), 'pane IDs changed during handoff'
            for pane, attributes in previous['panes'].items():
                if pane == server['sidebar']:
                    assert current[pane][0] != attributes[0], 'old sidebar process survived the upgrade'
                    assert current[pane][1:] == attributes[1:], 'sidebar geometry, focus or cwd changed'
                else:
                    assert current[pane] == attributes, 'agent or supporting process changed'
            assert layout(server) == previous['layout'], 'window layout or zoom changed'
            assert focus(server) == previous['focus'], 'client focus changed'
            assert agent_identity(server) == previous['agents'], 'agent process identity changed'
            assert sidebar_ready(server), 'installer returned before the replacement sidebar rendered'
        print('PASS installer hands off legacy and current sidebars on every server, retaining panes, agents, support, layout, zoom and focus')

        # A plugin/tool caller can name one server explicitly. --all still
        # reconciles every server affected by replacing their shared core.
        previous = [panes(s) for s in servers]
        replacement = core.with_name('replacement')
        shutil.copy2(core, replacement)
        replacement.replace(core)
        run(str(core), 'daemon', 'ensure', '--all', environment=primary['env'])
        for server, old in zip(servers, previous):
            current = panes(server)
            assert current[server['sidebar']][0] != old[server['sidebar']][0]
            for pane in current:
                if pane != server['sidebar']:
                    assert current[pane] == old[pane], 'shared-core handoff changed another task process'
            assert sidebar_ready(server)
        print('PASS shared-core handoff reaches every server even with an explicit caller server')

        after = [panes(s) for s in servers]
        run(str(core), 'daemon', 'ensure', '--all')
        for server, previous in zip(servers, after):
            run(str(core), 'daemon', 'ensure', environment=server['env'])
            assert panes(server) == previous, 'repeating runtime handoff restarted a current process'
        print('PASS repeated install/plugin handoff preserves current sidebar processes')

        pane = primary['sidebar']
        rows = tmux(primary, 'capture-pane', '-p', '-t', pane).splitlines()
        row = next(i for i, line in enumerate(rows) if 'Task target' in line)
        assert tmux(primary, 'list-clients', '-F', '#{pane_id}') == primary['main']
        os.write(master, f'\x1b[<35;5;{row + 1}M'.encode())
        drain(.1)
        os.write(master, f'\x1b[<0;5;{row + 1}M\x1b[<0;5;{row + 1}m'.encode())
        wait_for(lambda: tmux(primary, 'list-clients', '-F', '#{pane_id}') == target,
                 'the first click after upgrade did not focus the target agent')
        errors = root / 'state/tmux-agent-workbench/popup-actions.log'
        assert not errors.exists() or 'sidebar-error' not in errors.read_text(), 'sidebar failed after upgrade'
        print('PASS the first real mouse click after atomic upgrade switches to the target agent')
    finally:
        for server in servers:
            status = subprocess.run([str(core), 'daemon', 'status'], env=server['env'],
                                    text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            if status.returncode == 0:
                pid = str(json.loads(status.stdout)['pid'])
                subprocess.run([str(core), 'daemon', 'stop'], env=server['env'],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                deadline = time.monotonic() + 5
                while True:
                    process = subprocess.run(['ps', '-o', 'stat=', '-p', pid], text=True,
                                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                    if not process.stdout.strip() or process.stdout.lstrip().startswith('Z'):
                        break
                    assert time.monotonic() < deadline, 'upgraded daemon did not exit before cleanup'
                    time.sleep(.05)
        os.close(master)
        os.close(slave)
        if client is not None:
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)
        for server in servers:
            subprocess.run(['tmux', '-S', server['socket'], 'kill-server'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
