"""Route task windows from any pane without leaving agents or stale targets behind."""
from concurrent.futures import ThreadPoolExecutor
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
with tempfile.TemporaryDirectory(prefix='wb-task-placement-', dir='/tmp') as root:
    root = str(Path(root).resolve())
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX=socket + ',0,0',
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket, CODEX_HOME=root + '/codex')
    for key in ('TMUX_PANE', 'WORKBENCH_POPUP', 'WORKBENCH_SOURCE_PANE'):
        env.pop(key, None)
    for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
        env[key] = root + '/' + key
    env['XDG_RUNTIME_DIR'] = root
    config = Path(env['XDG_CONFIG_HOME'], 'tmux-agent-workbench')
    config.mkdir(parents=True)
    (config / 'config.toml').write_text('[notifications]\nenabled = false\n')

    def tmux(*args):
        return subprocess.check_output(['tmux', '-u', '-S', socket, *args], env=env,
                                       text=True, timeout=10).rstrip('\n')

    def core(*args, data=None, check=True):
        result = subprocess.run([binary, *args], env=env, text=True, capture_output=True,
                                input=json.dumps(data) if data is not None else None, timeout=10)
        if check:
            assert result.returncode == 0, (args, result.stderr)
        return result

    def snapshot():
        return json.loads(core('snapshot', '--json').stdout)

    def live(pane):
        return next((agent for agent in snapshot()['agents']
                     if not agent['exited'] and agent['target']['pane_id'] == pane), None)

    def at(pane):
        return tmux('display-message', '-p', '-t', pane,
                    '#{session_id}\x1f#{session_name}\x1f#{window_id}\x1f#{window_name}').split('\x1f')

    def wait(predicate, label):
        deadline = time.monotonic() + 12
        while not predicate():
            assert time.monotonic() < deadline, (label, snapshot())
            time.sleep(.05)

    def bind(pane, session=None):
        wait(lambda: live(pane) is not None, 'agent discovery')
        session = session or str(uuid.uuid4())
        core('hook', 'ingest', 'codex', 'SessionStart', data={
            'session_id': session, 'agent_pid': live(pane)['process']['pid']})
        wait(lambda: live(pane).get('hook_session_id') == session, 'hook binding')
        return session

    def set_title(pane, session, name=None, group=None):
        args = ['title', 'set'] + ([name] if name is not None else [])
        if group is not None:
            args += ['--session', group]
        result = json.loads(core(*args, '--pane', pane, '--session-id', session).stdout)
        current = live(pane)
        actual = at(pane)
        assert current['target']['session_id'] == actual[0], (result, current, actual)
        assert current['target']['session_name'] == actual[1], (result, current, actual)
        assert current['target']['window_id'] == actual[2], (result, current, actual)
        assert current['target']['window_name'] == actual[3], (result, current, actual)
        if name is not None:
            assert current['thread_name'] == current['label'] == actual[3] == name, current
        if group is not None:
            assert actual[1] == group, actual
        return result

    clients = []
    try:
        first = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'launch-pad', '-n', 'zsh',
                     '-x', '160', '-y', '40', '-P', '-F', '#{pane_id}',
                     'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture))
        tmux('set-option', '-g', 'automatic-rename', 'off')
        # Split before daemon startup to exercise adoption of an existing layout.
        second = tmux('split-window', '-d', '-h', '-t', first, '-P', '-F', '#{pane_id}',
                      'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture))
        support = tmux('split-window', '-d', '-v', '-t', first, '-P', '-F', '#{pane_id}',
                       'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture) + ' exec')
        tmux('set-option', '-p', '-t', support, '@pane_role', 'dev-server')
        # An expired title lease in a reused pane must not displace the oldest
        # live agent or leave its supporting panes with the wrong task.
        tmux('set-option', '-w', '-t', first, '@workbench_task_title', json.dumps({
            'owner': {'pane': second, 'session': 'expired', 'process': {
                'pid': 99999999, 'started_at_ticks': 0, 'executable': '/expired-agent'}},
            'initial_name': 'zsh', 'title': None, 'task_seen': False, 'repair_used': False}))
        tmux('new-session', '-d', '-s', '终端工具', '-n', 'human-shell')
        with ExitStack() as stack:
            stack.enter_context(running_daemon(binary, env))
            wait(lambda: live(first) and live(second) and at(first)[2] != at(second)[2],
                 'existing independent agents split')
            assert at(first)[2] == at(support)[2]
            sessions = {first: bind(first), second: bind(second)}
            second_identity = live(second)['process']
            cwd = tmux('display-message', '-p', '-t', second, '#{pane_current_path}')
            set_title(first, sessions[first], '同步任务标题', '终端工具')
            set_title(second, sessions[second], '修复文档布局', 'Office研发')
            assert at(first)[1:] != at(second)[1:]
            assert live(second)['process'] == second_identity
            assert tmux('display-message', '-p', '-t', second, '#{pane_current_path}') == cwd
            assert at(support)[1:3] == at(first)[1:3]
            assert 'launch-pad' not in tmux('list-sessions', '-F', '#{session_name}').splitlines()
            assert len(tmux('list-windows', '-t', 'Office研发', '-F', '#{window_id}').splitlines()) == 1
            print('PASS existing panes separate, reuse/create groups, retain support/process/cwd, remove placeholders')

            # A later agent launched alongside a named task must not inherit its name.
            third = tmux('split-window', '-d', '-h', '-t', first, '-P', '-F', '#{pane_id}',
                         'sleep 300')
            viewer = subprocess.Popen(['tmux', '-S', socket, '-C', 'attach-session', '-t', at(first)[0]],
                                      env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL)
            clients.append(viewer)
            tmux('select-pane', '-t', third)
            wait(lambda: third in tmux('list-clients', '-F', '#{pane_id}').splitlines(), 'new pane focused')
            tmux('respawn-pane', '-k', '-t', third,
                 'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture))
            wait(lambda: live(third) and at(third)[2] != at(first)[2], 'later agent split')
            assert third in tmux('list-clients', '-F', '#{pane_id}').splitlines()
            sessions[third] = bind(third)
            assert at(first)[3] == '同步任务标题'
            hook = json.loads(core('hook', 'ingest', 'codex', 'UserPromptSubmit', data={
                'session_id': sessions[third], 'agent_pid': live(third)['process']['pid']}).stdout)
            context = hook['hookSpecificOutput']['additionalContext']
            assert '--session' in context and '终端工具' in context and 'Office研发' in context, context
            set_title(third, sessions[third], '验证文档选择', 'Office研发')
            set_title(third, sessions[third], '验证选择与光标')
            assert at(third)[1] == 'Office研发'
            # Group-only edits keep task names; set also handles an existing name.
            set_title(third, sessions[third], group='终端工具')
            assert at(third)[3] == '验证选择与光标'
            set_title(third, sessions[third], group='Office研发')
            assert core('hook', 'ingest', 'codex', 'UserPromptSubmit', data={
                'session_id': sessions[third], 'agent_pid': live(third)['process']['pid']}).stdout.strip() == '{}'
            print('PASS later agents get their own window and semantic routing context; set updates names or groups')

            # Concurrent callers selecting the same new group create one session.
            with ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(lambda p: set_title(p, sessions[p], group="工具 O'Reilly"),
                                        (second, third)))
            assert at(second)[0] == at(third)[0] and at(second)[2] != at(third)[2], results
            assert len(tmux('list-windows', '-t', at(second)[0], '-F', '#{window_id}').splitlines()) == 2
            for pane in (first, second, third):
                before = at(pane)
                failed = core('title', 'set', 'Invalid move', '--session', 'invalid:group',
                              '--pane', pane, '--session-id', sessions[pane], check=False)
                assert failed.returncode and at(pane) == before
            print('PASS concurrent group creation is serialized and invalid labels leave the layout intact')

            # An attached human follows their exact pane when the last window moves.
            client = subprocess.Popen(['tmux', '-S', socket, '-C', 'attach-session', '-t', at(first)[0]],
                                      env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL)
            clients.append(client)
            tmux('select-pane', '-t', first)
            wait(lambda: first in tmux('list-clients', '-F', '#{pane_id}').splitlines(), 'client attached')
            set_title(first, sessions[first], group='终端体验')
            wait(lambda: first in tmux('list-clients', '-F', '#{pane_id}').splitlines(), 'client follows moved pane')
            assert client.poll() is None
            assert '终端工具' in tmux('list-sessions', '-F', '#{session_name}').splitlines()  # human shell stays

            sidebar = tmux('split-window', '-d', '-h', '-l', '50', '-t', first, '-P', '-F', '#{pane_id}',
                           shlex.quote(binary) + ' sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            wait(lambda: all(text in tmux('capture-pane', '-p', '-t', sidebar)
                             for text in ('同步任务标题', '修复文档布局', '验证选择与光标')),
                 'actual sidebar reflects routed agents')
            stack.close()
            stack.enter_context(running_daemon(binary, env))
            wait(lambda: all(live(p) and live(p)['target']['session_name'] == at(p)[1]
                             for p in (first, second, third)), 'recovery targets')
            assert live(third)['thread_name'] == '验证选择与光标'
            assert core('hook', 'ingest', 'codex', 'UserPromptSubmit', data={
                'session_id': sessions[third], 'agent_pid': live(third)['process']['pid']}).stdout.strip() == '{}'
            print('PASS client focus, rendered sidebar, and recovered names/targets follow placement')

            # Workspace ownership follows window metadata, independently of its
            # display session's root. This keeps task tools in the right checkout.
            workspace = Path(root, 'workspaces', 'original')
            task_root = workspace / 'tasks' / 'layout'
            task_root.mkdir(parents=True)
            tmux('new-session', '-d', '-s', 'managed-origin')
            set_title(second, sessions[second], group='managed-origin')
            tmux('set-option', '-t', 'managed-origin', '@workbench_workspace_root', str(workspace))
            tmux('set-option', '-w', '-t', second, '@workbench_task_root', str(task_root))
            tmux('set-option', '-w', '-t', second, '@workbench_task_name', 'layout')
            tmux('new-session', '-d', '-s', 'managed-destination')
            tmux('set-option', '-t', 'managed-destination', '@workbench_workspace_root', root + '/other-workspace')
            set_title(second, sessions[second], group='managed-destination')
            repository = str(Path(__file__).resolve().parents[1])
            context = subprocess.run(['/bin/sh', '-eu', '-c',
                '. "$1/lib/resolve-session.sh"\n. "$1/lib/task-context.sh"\n'
                'workbench_task_context\nprintf "%s\\n" "$wb_workspace" "$wb_task_root" "$wb_window"',
                'placement-context', repository], env=dict(env, TMUX_PANE=second),
                text=True, capture_output=True, timeout=10)
            assert context.returncode == 0, context.stderr
            assert context.stdout.splitlines() == [str(workspace), str(task_root), at(second)[2]], context.stdout
            print('PASS managed task tools retain original workspace ownership after grouping')

            fresh = str(uuid.uuid4())
            core('hook', 'ingest', 'codex', 'SessionStart', data={
                'session_id': fresh, 'agent_pid': live(third)['process']['pid'], 'source': 'clear'})
            sessions[third] = fresh
            wait(lambda: live(third).get('hook_session_id') == fresh and not live(third).get('thread_name'),
                 'clear resets routed task')
            hook = json.loads(core('hook', 'ingest', 'codex', 'UserPromptSubmit', data={
                'session_id': fresh, 'agent_pid': live(third)['process']['pid']}).stdout)
            assert '--session' in hook['hookSpecificOutput']['additionalContext'], hook
            set_title(third, fresh, '重新归类新任务', '新任务方向')
            print('PASS clear renews grouping and routes the same running CLI to a different direction')

            # Explicit team panes retain the dedicated layout they own.
            team = tmux('new-window', '-d', '-t', at(first)[0], '-n', 'team', '-P', '-F', '#{pane_id}', 'sleep 300')
            tmux('set-option', '-p', '-t', team, '@agent_team', 'test-team')
            tmux('respawn-pane', '-k', '-t', team, 'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture))
            worker = tmux('split-window', '-d', '-t', team, '-P', '-F', '#{pane_id}', 'sleep 300')
            tmux('set-option', '-p', '-t', worker, '@agent_team', 'test-team')
            tmux('respawn-pane', '-k', '-t', worker, 'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture))
            wait(lambda: live(team) and live(worker), 'team discovery')
            time.sleep(1.2)
            assert at(team)[2] == at(worker)[2]
            print('PASS explicitly marked team layouts remain together')
    finally:
        for client in clients:
            if client.poll() is None:
                client.stdin.write(b'detach-client\n')
                client.stdin.flush()
                client.wait(timeout=5)
            client.stdin.close()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], capture_output=True, timeout=5)
