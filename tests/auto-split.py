"""Check task-directed splitting on a private tmux server with mock agents."""
from contextlib import ExitStack
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

binary, fixture = (str(Path(arg).resolve()) for arg in sys.argv[1:])
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
    bin_dir = Path(root, 'bin')
    bin_dir.mkdir()
    (bin_dir / 'tmux-agent-workbench').symlink_to(repo / 'bin/tmux-agent-workbench-cli')
    env['PATH'] = str(bin_dir) + os.pathsep + env['PATH']
    config = Path(env['XDG_CONFIG_HOME'], 'tmux-agent-workbench')
    config.mkdir(parents=True)
    (config / 'config.toml').write_text('[notifications]\nenabled = false\n[auto_split]\nenabled = true\n')

    def tmux(*args):
        return subprocess.check_output(['tmux', '-u', '-S', socket, *args], env=env,
                                       text=True, timeout=10).rstrip('\n')

    def core(*args, data=None):
        result = subprocess.run([binary, *args], env=env, text=True, capture_output=True,
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
        return core('hook', 'ingest', 'codex', event,
                    data={'session_id': session, 'agent_pid': agent['process']['pid']})

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
        incoming = tmux('new-session', '-d', '-s', 'launch-pad', '-n', 'codex',
                        '-P', '-F', '#{pane_id}', command)
        tmux('new-session', '-d', '-s', 'unrelated', '-n', 'shell', 'sleep 300')
        env['TMUX'] = socket + ',' + tmux('display-message', '-p', '#{pid}') + ',0'
        tmux('set-option', '-w', '-t', existing[3], '@agent_focus', '1')
        with ExitStack() as stack:
            stack.enter_context(running_daemon(binary, env))
            wait(lambda: all(live(p) for p in existing + [incoming]), 'mock agent discovery')
            sessions = {pane: str(uuid.uuid4()) for pane in existing + [incoming]}
            topics = ['文字光标', '文本样式', '协作评论', '图片排版', '页面布局']
            for pane, topic in zip(existing, topics):
                report(pane, sessions[pane], 'SessionStart')
                wait(lambda: live(pane).get('hook_session_id') == sessions[pane], 'mock binding')
                result = title(pane, sessions[pane], topic, 'Office文档')
                assert result['status'] == 'renamed', result  # Five existing tasks are allowed.
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
            assert result['status'] == 'needs_split' and result['current_tasks'] == result['limit'] == 5, result
            assert 'exactly one native subagent' in result['instruction'] and '--scope-session' in result['instruction']
            source = result['source_session']
            ticket = result['ticket']
            assert location(incoming) == before_location
            assert title(incoming, sessions[incoming], '图片拖拽定位', 'Office文档')['status'] == 'split_pending'
            assert len(core('auto-split', 'status')['pending']) == 1

            # Simulate the native worker's result only on this disposable server.
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
            print('PASS fifth task allowed; sixth triggers one scoped split; incoming task joins semantic child')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env, capture_output=True, timeout=5)
