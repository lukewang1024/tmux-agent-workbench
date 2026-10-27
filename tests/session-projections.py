"""A thread switch must converge across every projection of the agent state."""
from contextlib import ExitStack
from concurrent.futures import ThreadPoolExecutor
import fcntl
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
with tempfile.TemporaryDirectory(prefix='wb-session-projections-', dir='/tmp') as root:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TERM='xterm-256color', TMUX=socket + ',0,0',
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket, CODEX_HOME=root + '/codex')
    for key in ('TMUX_PANE', 'WORKBENCH_POPUP', 'WORKBENCH_SOURCE_PANE'):
        env.pop(key, None)
    for key in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = root + '/' + key
    env['XDG_RUNTIME_DIR'] = root
    config = Path(env['XDG_CONFIG_HOME'], 'tmux-agent-workbench')
    config.mkdir(parents=True)
    (config / 'config.toml').write_text('[notifications]\nenabled = false\n')

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env,
                                       text=True, timeout=10).strip()

    def core(*args, data=None, check=True):
        result = subprocess.run([binary, *args], env=env, capture_output=True,
                                input=json.dumps(data) if data is not None else None,
                                text=True, timeout=10)
        if check:
            assert result.returncode == 0, (args, result.stderr)
        return result

    def snapshot():
        return json.loads(core('snapshot', '--json').stdout)

    def agent():
        return next((item for item in snapshot()['agents']
                     if item['target']['pane_id'] == main and not item['exited']), None)

    def wait(predicate, label):
        deadline = time.monotonic() + 10
        while not predicate():
            assert time.monotonic() < deadline, (label, agent(), screen())
            time.sleep(.05)

    def screen():
        return tmux('capture-pane', '-p', '-t', sidebar)

    def hook(session, event='SessionStart', **extra):
        return core('hook', 'ingest', 'codex', event,
                    data={'session_id': session, 'agent_pid': agent()['process']['pid'],
                          **extra})

    def pane_title(value):
        tty = tmux('display-message', '-p', '-t', main, '#{pane_tty}')
        with open(tty, 'w') as stream:
            stream.write('\x1b]2;' + value + '\x07')
            stream.flush()

    def osc(session):
        pane_title('codex | Ready | ' + session[:32] + '...')

    def name_command(session, value, action='set'):
        core('title', action, value, '--pane', main, '--session-id', session)
        # Successful command completion includes shared state publication.
        current = agent()
        assert current['thread_name'] == value and current['label'] == value, current
        assert current['target']['window_name'] == value, current
        assert tmux('display-message', '-p', '-t', main, '#{window_name}') == value
        assert value in tmux('display-message', '-p', '-t', main,
                             '#{E:window-status-current-format}')
        wait(lambda: value in screen(), 'CLI name rendered in sidebar')

    def converged(session, name, obsolete):
        current = agent()
        if current is None or not session.startswith(current.get('hook_session_id', '!')):
            return False
        if current.get('thread_name') != name or current.get('task_progress'):
            return False
        if current.get('attention') or obsolete in current['label'] or obsolete in screen():
            return False
        title = name or 'zsh'
        if tmux('display-message', '-p', '-t', main, '#{window_name}') != title:
            return False
        owned = json.loads(tmux('show-option', '-wqv', '-t', main, '@workbench_task_title'))
        if not session.startswith(owned['owner']['session']):
            return False
        # Generic window names are rendered using the session name.
        if (name or 'projection-test') not in screen():
            return False
        checkpoints = list(Path(env['XDG_STATE_HOME']).glob('tmux-agent-workbench/servers/*.json'))
        return any(any(session.startswith(runtime.get('hook_session_id') or '!')
                       and runtime.get('thread_name') == name
                       for runtime in json.loads(path.read_text())['runtimes'])
                   for path in checkpoints)

    client = None
    try:
        main = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'projection-test',
                    '-n', 'zsh', '-x', '160', '-y', '40', '-P', '-F', '#{pane_id}',
                    'env WORKBENCH_FIXTURE_SECONDS=300 ' + shlex.quote(fixture))
        tmux('set-option', '-g', 'automatic-rename', 'off')
        with ExitStack() as stack:
            stack.enter_context(running_daemon(binary, env))
            sidebar = tmux('split-window', '-d', '-h', '-l', '50', '-t', main,
                           '-P', '-F', '#{pane_id}', shlex.quote(binary) + ' sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            # Control mode marks the sidebar visible and keeps its polling active.
            client = subprocess.Popen(['tmux', '-S', socket, '-C', 'attach-session',
                                       '-t', 'projection-test'], env=env, stdin=subprocess.PIPE,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait(lambda: agent() is not None, 'agent discovery')
            manual = str(uuid.uuid4())
            hook(manual)
            name_command(manual, '统一名称 Alpha')
            hook(manual, 'PostToolUse', thread_name='Cached provider name')
            index = Path(env['CODEX_HOME'], 'session_index.jsonl')
            index.parent.mkdir(parents=True, exist_ok=True)
            index.write_text(json.dumps({'id': manual, 'thread_name': 'Cached provider name'}) + '\n')
            time.sleep(2.2)  # Include the detector's next thread-index refresh.
            wait(lambda: converged(manual, '统一名称 Alpha', 'Cached provider'),
                 'CLI name survives provider metadata')
            window = tmux('display-message', '-p', '-t', main, '#{window_id}')[1:]
            lock, = Path(env['XDG_RUNTIME_DIR'], 'tmux-agent-workbench').glob(f'task-title-*-{window}.lock')
            with ThreadPoolExecutor(max_workers=1) as pool:
                with lock.open('r+') as held:
                    fcntl.flock(held, fcntl.LOCK_EX)
                    naming = pool.submit(name_command, manual, 'CLI after busy lock', 'rename')
                    time.sleep(.1)
                    fcntl.flock(held, fcntl.LOCK_UN)
                naming.result(timeout=10)
            checkpoint, = Path(env['XDG_STATE_HOME']).glob('tmux-agent-workbench/servers/*.json')
            backup = checkpoint.with_suffix('.before-name')
            checkpoint.rename(backup)
            checkpoint.mkdir()
            try:
                failed = core('title', 'rename', 'Recovery write failed', '--pane', main,
                              '--session-id', manual, check=False)
                assert failed.returncode != 0 and 'could not save recovery state' in failed.stderr, failed
                current = agent()
                assert current['thread_name'] == current['target']['window_name'] == 'Recovery write failed', current
            finally:
                checkpoint.rmdir()
                backup.rename(checkpoint)
            name_command(manual, "统一名称 Beta O'Reilly", action='rename')
            stack.close()
            stack.enter_context(running_daemon(binary, env))
            wait(lambda: converged(manual, "统一名称 Beta O'Reilly", 'Cached provider'),
                 'CLI rename survives daemon recovery')
            fresh = str(uuid.uuid4())
            hook(fresh, source='clear')
            wait(lambda: converged(fresh, None, '统一名称'), 'clear resets CLI name everywhere')
            print('PASS: title set/rename updates sidebar and statusbar, persists, and clears together')
            for order in ('hook-first', 'osc-first'):
                old, new = str(uuid.uuid4()), str(uuid.uuid4())
                old_name = 'Obsolete ' + order
                hook(old, thread_name=old_name)
                osc(old)
                wait(lambda: old_name in screen(), 'initial shared name')
                if order == 'osc-first':
                    pane_title('codex | Ready | ' + old_name + ' | 2/5')
                    wait(lambda: agent().get('task_progress') == {'completed': 2, 'total': 5},
                         'previous task progress')
                hook(old, 'PermissionRequest')
                wait(lambda: bool(agent().get('attention')), 'previous task attention')
                if order == 'hook-first':
                    hook(new, source='clear')
                else:
                    osc(new)
                wait(lambda: converged(new, None, old_name), order + ' clear projections')
                # Recovery while the new task still has no name or hook must
                # neither restore old data nor follow the old OSC after a hook.
                stack.close()
                stack.enter_context(running_daemon(binary, env))
                wait(lambda: converged(new, None, old_name), order + ' clear recovery')
                if order == 'hook-first':
                    osc(new)
                else:
                    hook(new, source='clear')
                hook(new, 'UserPromptSubmit', thread_name='Current ' + order)
                wait(lambda: converged(new, 'Current ' + order, old_name),
                     order + ' named projections')
            # Persisted state and the same live sidebar must survive daemon replacement.
        with running_daemon(binary, env):
            wait(lambda: converged(new, 'Current osc-first', 'Obsolete'),
                 'restart projections')
        print('PASS: clear synchronizes window, sidebar, snapshot and checkpoint in both event orders')
    finally:
        if client is not None:
            client.terminate()
            client.wait(timeout=5)
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
