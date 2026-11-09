"""Check task-directed splitting on a private tmux server with mock agents."""
from contextlib import ExitStack
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon

binary, fixture = (str(Path(arg).resolve()) for arg in sys.argv[1:3])
legacy_daemon = '--daemon-auto-split-off' in sys.argv[3:] or '--daemon-bin' in sys.argv[3:]
daemon_binary = (str(Path(sys.argv[sys.argv.index('--daemon-bin') + 1]).resolve())
                 if '--daemon-bin' in sys.argv[3:] else binary)
incoming_claude = '--incoming-claude' in sys.argv[3:]
incoming_trae = '--incoming-trae' in sys.argv[3:]
incoming_opencode = '--incoming-opencode' in sys.argv[3:]
assert sum((incoming_claude, incoming_trae, incoming_opencode)) <= 1
repo = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='wb-auto-split-', dir='/tmp') as root:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX=socket + ',0,0',
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket, CODEX_HOME=root + '/codex',
               TMUX_AGENT_WORKBENCH_BIN=binary, XDG_RUNTIME_DIR=root)
    for key in ('TMUX_PANE', 'WORKBENCH_POPUP', 'WORKBENCH_SOURCE_PANE', 'AGENT_TEAM_DIR'):
        env.pop(key, None)
    for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
        env[key] = root + '/' + key
    state_real = Path(root, 'state-real')
    state_real.mkdir()
    Path(env['XDG_STATE_HOME']).symlink_to(state_real, target_is_directory=True)
    bin_dir = Path(root, 'bin')
    bin_dir.mkdir()
    (bin_dir / 'tmux-agent-workbench').symlink_to(repo / 'bin/tmux-agent-workbench-cli')
    codex_stub = bin_dir / 'codex'
    codex_stub.write_text('#!/bin/sh\ncase "$1" in\n  exec) printf \'%s\\n\' \'{"type":"thread.started","thread_id":"00000000-0000-4000-8000-000000000001"}\' ;;\n  queue) exit 0 ;;\n  --no-alt-screen) exec sleep 300 ;;\nesac\nexit 0\n')
    codex_stub.chmod(0o755)
    incoming_kind = 'claude' if incoming_claude else 'trae' if incoming_trae else 'opencode' if incoming_opencode else 'codex'
    incoming_fixture = bin_dir / ('traex' if incoming_trae else incoming_kind)
    if incoming_kind != 'codex':
        shutil.copyfile(fixture, incoming_fixture)
        incoming_fixture.chmod(0o755)
    env['PATH'] = str(bin_dir) + os.pathsep + env['PATH']
    config = Path(env['XDG_CONFIG_HOME'], 'tmux-agent-workbench')
    config.mkdir(parents=True)
    (config / 'config.toml').write_text('[notifications]\nenabled = false\n[auto_split]\nenabled = true\n')
    daemon_env = env.copy()
    if legacy_daemon:
        daemon_env['XDG_CONFIG_HOME'] = root + '/daemon-config'
        daemon_config = Path(daemon_env['XDG_CONFIG_HOME'], 'tmux-agent-workbench')
        daemon_config.mkdir(parents=True)
        (daemon_config / 'config.toml').write_text('[notifications]\nenabled = false\n')

    def tmux(*args):
        return subprocess.check_output(['tmux', '-u', '-S', socket, *args], env=env,
                                       text=True, timeout=10).rstrip('\n')

    def core(*args, data=None, pane=None):
        command_env = dict(env, TMUX_PANE=pane) if pane is not None else env
        result = subprocess.run([binary, *args], env=command_env, text=True, capture_output=True,
                                input=json.dumps(data) if data is not None else None, timeout=10)
        assert result.returncode == 0, (args, result.stderr)
        return json.loads(result.stdout)

    def snapshot():
        return core('snapshot', '--json')

    def live(pane):
        return next((a for a in snapshot()['agents']
                     if not a['exited'] and a['target']['pane_id'] == pane), None)

    def wait(predicate, label):
        deadline = time.monotonic() + 20
        while not predicate():
            assert time.monotonic() < deadline, (label, snapshot())
            time.sleep(.05)

    def report(pane, session, event='UserPromptSubmit'):
        agent = live(pane)
        return core('hook', 'ingest', agent['kind'], event,
                    data={'session_id': session, 'agent_pid': agent['process']['pid']},
                    pane=pane if agent['kind'] == 'claude' else None)

    def title(pane, session, name, direction):
        return core('title', 'set', name, '--session', direction,
                    '--pane', pane, '--session-id', session)

    def location(pane):
        return tmux('display-message', '-p', '-t', pane,
                    '#{session_id}\x1f#{session_name}\x1f#{window_id}').split('\x1f')

    try:
        command = 'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture)
        existing = [tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'Office文档', '-n', 'codex',
                         '-P', '-F', '#{pane_id}', command)]
        for _ in range(4):
            existing.append(tmux('new-window', '-d', '-t', 'Office文档:', '-n', 'codex',
                                 '-P', '-F', '#{pane_id}', command))
        tmux('new-window', '-d', '-t', 'Office文档:', '-n', 'human-shell', 'sleep 300')
        incoming_command = 'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(str(incoming_fixture)) if incoming_kind != 'codex' else command
        incoming = tmux('new-session', '-d', '-s', 'launch-pad', '-n', incoming_kind,
                        '-P', '-F', '#{pane_id}', incoming_command)
        tmux('new-session', '-d', '-s', 'unrelated', '-n', 'shell', 'sleep 300')
        env['TMUX'] = socket + ',' + tmux('display-message', '-p', '#{pid}') + ',0'
        tmux('set-option', '-w', '-t', existing[3], '@agent_focus', '1')
        with ExitStack() as stack:
            stack.enter_context(running_daemon(daemon_binary, daemon_env))
            daemon_pid = core('daemon', 'status')['pid']
            wait(lambda: all(live(p) for p in existing + [incoming]), 'mock agent discovery')
            sessions = {pane: str(uuid.uuid4()) for pane in existing + [incoming]}
            topics = ['文字光标', '文本样式', '协作评论', '图片排版', '页面布局']
            for pane, topic in zip(existing, topics):
                report(pane, sessions[pane], 'SessionStart')
                wait(lambda: live(pane).get('hook_session_id') == sessions[pane], 'mock binding')
                result = title(pane, sessions[pane], topic, 'Office文档')
                assert result['status'] == 'renamed', result  # Five existing tasks are allowed.
            assert live(incoming)['kind'] == incoming_kind
            report(incoming, sessions[incoming], 'SessionStart')
            wait(lambda: live(incoming).get('hook_session_id') == sessions[incoming], 'incoming binding')
            context = report(incoming, sessions[incoming])['hookSpecificOutput']['additionalContext']
            assert 'Office文档' in context and '图片排版' in context and '5 live task windows' in context, context
            incoming_pid = live(incoming)['process']['pid']

            # A full unrelated direction must not trigger a global split.
            result = title(incoming, sessions[incoming], '图片拖拽定位', '独立临时方向')
            assert result['status'] == 'renamed', result
            assert core('auto-split', 'status')['pending'] == []
            before_location = location(incoming)
            result = title(incoming, sessions[incoming], '图片拖拽定位', 'Office文档')
            assert result['status'] == 'manager_pending' and result['current_tasks'] == result['limit'] == 5, result
            assert 'single machine-wide coding-agent manager session' in result['instruction']
            assert 'Do not spawn or delegate to another agent' in result['instruction']
            source = result['source_session']
            ticket = result['ticket']
            assert location(incoming) == before_location
            repeated = title(incoming, sessions[incoming], '图片拖拽定位', 'Office文档')
            assert repeated['status'] == 'manager_pending' and repeated['ticket'] == ticket
            assert len(core('auto-split', 'status')['pending']) == 1
            wait(lambda: core('auto-split', 'status')['manager']['submitted_ticket_count'] == 1,
                 'singleton manager queue')
            manager = core('auto-split', 'status')['manager']
            manager_pane = tmux('list-panes', '-s', '-t', manager['session_name'],
                                '-F', '#{pane_id}').splitlines()[0]
            access = tmux('show-option', '-qv', '-p', '-t', manager_pane,
                          '@workbench_manager_access')
            assert access.startswith('danger-full-access-v1:'), access
            assert env['XDG_STATE_HOME'] + '/tmux-agent-workbench' in access, access
            assert str(Path(env['XDG_STATE_HOME']).resolve()) in access, access
            assert str(Path(socket).parent.resolve()) in access, access

            # An existing manager thread with stale sandbox access is replaced in
            # the same reserved session, then receives the still-pending ticket.
            manager_state_file = (Path(env['XDG_STATE_HOME']).resolve() /
                                  'tmux-agent-workbench/manager/state.json')
            manager_state = json.loads(manager_state_file.read_text())
            manager_state['access_profile'] = 'stale-profile'
            manager_state_file.write_text(json.dumps(manager_state))
            previous_manager_pid = int(tmux('display-message', '-p', '-t', manager_pane, '#{pane_pid}'))
            retried = title(incoming, sessions[incoming], '图片拖拽定位', 'Office文档')
            assert retried['status'] == 'manager_pending' and retried['ticket'] == ticket, retried
            wait(lambda: int(tmux('display-message', '-p', '-t', manager_pane, '#{pane_pid}')) !=
                 previous_manager_pid, 'manager sandbox profile migration')
            assert len([name for name in tmux('list-sessions', '-F', '#{session_name}').splitlines()
                        if name.startswith('__workbench_manager_')]) == 1
            assert core('auto-split', 'status')['manager']['last_error'] is None

            # Simulate the singleton manager's scoped result on this disposable server.
            helper = str(repo / 'bin/wb-reorganise')
            before = json.loads(subprocess.check_output([helper, 'snapshot'], env=env, text=True))
            moving = {location(pane)[2] for pane in existing[3:]}
            groups = []
            for session in before['sessions']:
                ids = [w['window_id'] for w in before['windows'] if w['session_id'] == session['session_id']]
                if session['session_id'] == source:
                    groups.append({'session': 'Office文档', 'windows': [w for w in ids if w not in moving]})
                    groups.append({'session': 'Office文档·布局', 'windows': [w for w in ids if w in moving]})
                else:
                    groups.append({'session': session['session_name'], 'windows': ids})
            snapshot_file, plan_file = Path(root, 'snapshot.json'), Path(root, 'plan.json')
            snapshot_file.write_text(json.dumps(before))
            plan_file.write_text(json.dumps({'groups': groups}))
            subprocess.check_output([helper, 'apply', '--snapshot', str(snapshot_file),
                                     '--plan', str(plan_file), '--scope-session', source], env=env, text=True)
            assert tmux('show-option', '-wqv', '-t', existing[3], '@agent_focus') == '1'
            assert tmux('list-windows', '-t', 'unrelated', '-F', '#{window_name}') == 'shell'
            assert 'human-shell' in tmux('list-windows', '-t', source, '-F', '#{window_name}')
            core('auto-split', 'finish', '--ticket', ticket, '--result', 'completed')
            wait(lambda: location(existing[3])[1] == 'Office文档·布局' and
                 live(existing[3])['target']['session_name'] == 'Office文档·布局', 'split observed')
            result = title(incoming, sessions[incoming], '图片拖拽定位', 'Office文档·布局')
            assert result['status'] == 'renamed' and location(incoming)[1] == 'Office文档·布局', result
            assert live(incoming)['process']['pid'] == incoming_pid
            assert core('auto-split', 'status')['pending'] == []
            assert core('daemon', 'status')['pid'] == daemon_pid
            print('PASS ' + incoming_kind + ' caller: fifth task allowed; sixth triggers one scoped split; incoming task joins semantic child; daemon kept running')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, capture_output=True, timeout=5)
