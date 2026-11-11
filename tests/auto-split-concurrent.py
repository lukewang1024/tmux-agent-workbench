"""Concurrent clients cannot both occupy a direction's last free slot."""
from contextlib import ExitStack
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon

binary, fixture = (str(Path(arg).resolve()) for arg in sys.argv[1:3])
daemon_binary = str(Path(sys.argv[3]).resolve()) if len(sys.argv) > 3 else binary
with tempfile.TemporaryDirectory(prefix='wb-split-race-', dir='/tmp') as root:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               TMUX_AGENT_WORKBENCH_BIN=binary, XDG_RUNTIME_DIR=root,
               WORKBENCH_FIXTURE_SECONDS='120')
    for name in ('TMUX', 'TMUX_PANE', 'WORKBENCH_POPUP', 'WORKBENCH_SOURCE_PANE', 'AGENT_TEAM_DIR'):
        env.pop(name, None)
    for name in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
        env[name] = root + '/' + name
    bin_dir = Path(root) / 'bin'
    bin_dir.mkdir()
    codex_stub = bin_dir / 'codex'
    codex_stub.write_text('#!/bin/sh\ncase "$1" in\n  exec) printf \'%s\\n\' \'{"type":"thread.started","thread_id":"00000000-0000-4000-8000-000000000001"}\' ;;\n  queue) exit 0 ;;\n  --no-alt-screen) exec sleep 300 ;;\nesac\nexit 0\n')
    codex_stub.chmod(0o755)
    env['PATH'] = str(bin_dir) + os.pathsep + env['PATH']
    config = Path(env['XDG_CONFIG_HOME']) / 'tmux-agent-workbench'
    config.mkdir(parents=True)
    (config / 'config.toml').write_text('[notifications]\nenabled = false\n[auto_split]\nenabled = true\n')
    daemon_env = dict(env, XDG_CONFIG_HOME=root + '/old-daemon-config')
    old_config = Path(daemon_env['XDG_CONFIG_HOME']) / 'tmux-agent-workbench'
    old_config.mkdir(parents=True)
    (old_config / 'config.toml').write_text('[notifications]\nenabled = false\n')

    def tmux(*args):
        return subprocess.check_output(['tmux', '-u', '-S', socket, *args], env=env,
                                       text=True, timeout=10).rstrip('\n')

    def core(*args, data=None):
        p = subprocess.run([binary, *args], env=env, text=True, capture_output=True,
                           input=json.dumps(data) if data is not None else None, timeout=15)
        assert p.returncode == 0, (args, p.stderr)
        return json.loads(p.stdout)

    def live(pane):
        return next((a for a in core('snapshot', '--json')['agents']
                     if not a['exited'] and a['target']['pane_id'] == pane), None)

    def wait(predicate):
        deadline = time.monotonic() + 20
        while not predicate():
            assert time.monotonic() < deadline, 'mock agent not discovered'
            time.sleep(.05)

    try:
        command = shlex.quote(fixture)
        existing = [tmux('-f', '/dev/null', 'new-session', '-d', '-s', '方向', '-n', 'codex',
                         '-P', '-F', '#{pane_id}', command)]
        for _ in range(3):
            existing.append(tmux('new-window', '-d', '-t', '方向:', '-n', 'codex',
                                 '-P', '-F', '#{pane_id}', command))
        incoming = [tmux('new-session', '-d', '-s', f'入口-{i}', '-n', 'codex',
                         '-P', '-F', '#{pane_id}', command) for i in range(2)]
        env['TMUX'] = socket + ',' + tmux('display-message', '-p', '#{pid}') + ',0'
        daemon_env['TMUX'] = env['TMUX']
        with running_daemon(daemon_binary, daemon_env), ExitStack() as resources:
            # Stop the fixture's tmux server before reaping the daemon. This
            # prevents late tmux jobs from writing into its temporary runtime
            # directory while TemporaryDirectory is removing it.
            resources.callback(subprocess.run, ['tmux', '-S', socket, 'kill-server'],
                               env=env, capture_output=True, timeout=5)
            wait(lambda: all(live(p) for p in existing + incoming))
            sessions = {p: str(uuid.uuid4()) for p in existing + incoming}
            for p in existing + incoming:
                core('hook', 'ingest', 'codex', 'SessionStart',
                     data={'session_id': sessions[p], 'agent_pid': live(p)['process']['pid']})
                wait(lambda: live(p).get('hook_session_id') == sessions[p])
            for index, p in enumerate(existing):
                result = core('title', 'set', f'现有任务{index}', '--session', '方向',
                              '--pane', p, '--session-id', sessions[p])
                assert result['status'] == 'renamed', result
            before = core('daemon', 'status')['pid']
            def place(index):
                p = incoming[index]
                return core('title', 'set', f'并发任务{index}', '--session', '方向',
                            '--pane', p, '--session-id', sessions[p])
            with ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(place, range(2)))
            assert sorted(r['status'] for r in results) == ['manager_pending', 'renamed'], results
            assert len(tmux('list-windows', '-t', '方向', '-F', '#{window_id}').splitlines()) == 5
            assert len(core('auto-split', 'status')['pending']) == 1
            assert core('daemon', 'status')['pid'] == before
            print('PASS concurrent placements keep the fifth slot unique without restarting the daemon')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, capture_output=True, timeout=5)
