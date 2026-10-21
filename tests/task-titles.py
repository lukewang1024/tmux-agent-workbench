"""Verify title hooks and ownership against an isolated, real tmux server."""
from concurrent.futures import ThreadPoolExecutor
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon


binary, fixture = map(lambda value: str(Path(value).resolve()), sys.argv[1:])

with tempfile.TemporaryDirectory(prefix='workbench-titles-', dir='/tmp') as root:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               XDG_CONFIG_HOME=root+'/config', XDG_STATE_HOME=root+'/state',
               XDG_CACHE_HOME=root+'/cache', XDG_RUNTIME_DIR=root,
               TMUX=socket+',0,0')
    detached_env = {key: value for key, value in env.items()
                    if key not in ('TMUX', 'TMUX_PANE', 'TMUX_AGENT_WORKBENCH_TMUX_SOCKET')}
    claude = root + '/claude'
    shutil.copyfile(fixture, claude)
    os.chmod(claude, 0o700)

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def core(*args, run_env=None, data=None, check=True):
        result = subprocess.run([binary, *args], env=run_env or env,
                                input=json.dumps(data) if data is not None else None,
                                capture_output=True, text=True, timeout=10)
        if check:
            assert result.returncode == 0, (args, result.stderr)
        return result

    def wait_agent(pane, previous_pid=None):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            snapshot = json.loads(core('snapshot', '--json').stdout)
            agents = [agent for agent in snapshot['agents']
                      if agent['target']['pane_id'] == pane and not agent['exited']]
            if len(agents) == 1 and agents[0]['process'] and agents[0]['process']['pid'] != previous_pid:
                return agents[0]
            time.sleep(.05)
        raise AssertionError(('agent not discovered', pane, snapshot))

    def spawn(name='zsh', split=None, kind='codex'):
        executable = claude if kind == 'claude' else fixture
        args = ['split-window', '-d', '-t', split] if split else ['new-window', '-d', '-n', name]
        pane = tmux(*args, '-P', '-F', '#{pane_id}',
                    'env', 'WORKBENCH_FIXTURE_SECONDS=300', executable)
        agent = wait_agent(pane)
        return {'pane': pane, 'session_id': str(uuid.uuid4()),
                'agent_pid': agent['process']['pid'], 'kind': kind}

    def hook(agent, event, run_env=None, **extra):
        data = {'session_id': agent['session_id'], 'agent_pid': agent['agent_pid'],
                'cwd': root, **extra}
        hook_env = run_env or (detached_env if agent['kind'] == 'codex' else dict(env, TMUX_PANE=agent['pane']))
        return json.loads(core('hook', 'ingest', agent['kind'], event,
                               run_env=hook_env, data=data).stdout)

    def title(agent, value=None, run_env=None, **extra):
        args = ['title', 'status'] if value is None else ['title', 'set', value]
        result = core(*args, '--pane', agent['pane'], '--session-id', agent['session_id'],
                      run_env=run_env or detached_env, check=False, **extra)
        if result.returncode:
            snapshot = json.loads(core('snapshot', '--json').stdout)
            tracked = [item for item in snapshot['agents'] if item['target']['pane_id'] == agent['pane']]
            queued = {}
            for path in Path(root, 'tmux-agent-workbench').glob('spool-*/**/*.json'):
                try:
                    queued[str(path.relative_to(root))] = path.read_text()[:1500]
                except OSError:
                    pass  # The daemon may have drained a queued report.
            raise AssertionError((args, agent, result.stderr, tracked,
                                  tmux('display-message', '-p', '-t', agent['pane'], '#{pane_title}'),
                                  tmux('show-option', '-wqv', '-t', agent['pane'], '@workbench_task_title'),
                                  queued))
        return json.loads(result.stdout)

    def name(agent):
        return tmux('display-message', '-p', '-t', agent['pane'], '#{window_name}')

    def thread_title(agent):
        tty = tmux('display-message', '-p', '-t', agent['pane'], '#{pane_tty}')
        with open(tty, 'w') as stream:
            stream.write('\x1b]2;codex | Ready | ' + agent['session_id'][:32] + '...\x07')
            stream.flush()
        deadline = time.monotonic() + 3
        while agent['session_id'][:32] not in tmux('display-message', '-p', '-t', agent['pane'], '#{pane_title}'):
            assert time.monotonic() < deadline
            time.sleep(.02)

    try:
        inspection = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'titles',
                          '-n', 'User inspection', '-P', '-F', '#{pane_id}', 'sleep', '300')
        tmux('set-option', '-g', 'automatic-rename', 'off')
        with running_daemon(binary, env):
            repair = spawn()
            startup = hook(repair, 'SessionStart')
            assert startup.get('hookSpecificOutput', {}).get('hookEventName') == 'SessionStart', (
                startup, json.loads(core('snapshot', '--json').stdout))
            assert hook(repair, 'Stop') == {}, 'startup without a user task must not continue'
            context = hook(repair, 'UserPromptSubmit')['hookSpecificOutput']['additionalContext']
            assert repair['pane'] in context and repair['session_id'] in context
            stripped_env = dict(env)
            for key in ('TMUX', 'TMUX_PANE', 'XDG_RUNTIME_DIR'):
                stripped_env.pop(key, None)
            stripped = hook(repair, 'UserPromptSubmit', run_env=stripped_env)
            assert repair['pane'] in stripped['hookSpecificOutput']['additionalContext']
            recovered = core('title', 'status', '--pane', repair['pane'], '--session-id', repair['session_id'],
                             run_env=stripped_env)
            assert json.loads(recovered.stdout)['status'] == 'pending'
            assert hook(repair, 'PostToolUse') == {}, 'no repeated tool-result reminder'
            assert hook(repair, 'Stop', stop_hook_active=True) == {}
            window_id = tmux('display-message', '-p', '-t', repair['pane'], '#{window_id}')[1:]
            lock_path, = Path(root, 'tmux-agent-workbench').glob(f'task-title-*-{window_id}.lock')
            with lock_path.open('r+') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                assert hook(repair, 'Stop') == {}, 'a busy naming lock must let work proceed'
            with ThreadPoolExecutor(max_workers=2) as pool:
                stops = list(pool.map(lambda _: hook(repair, 'Stop'), range(2)))
            assert sum(output.get('decision') == 'block' for output in stops) == 1, stops
            assert title(repair)['repair_used'] is True
            assert hook(repair, 'Stop') == {}

            # Match the real Codex workspace sandbox: correct hook parameters
            # do not grant Unix socket access. Diagnose both daemon discovery
            # and tmux verification, without changing the agent's permissions.
            if sys.platform == 'darwin' and shutil.which('codex'):
                sandbox_help = subprocess.run(['codex', 'sandbox', '--help'],
                                              capture_output=True, text=True)
                if '--allow-unix-socket' in sandbox_help.stdout:
                    def sandbox_title(agent, allowed=None, run_env=None):
                        args = ['codex', 'sandbox', '-P', ':workspace', '-C', root]
                        if allowed:
                            args += ['--allow-unix-socket', allowed]
                        return subprocess.run(args + ['--', binary, 'title', 'set',
                                                     '--pane', agent['pane'],
                                                     '--session-id', agent['session_id'],
                                                     'Approved sandbox title'],
                                              env=run_env or detached_env,
                                              capture_output=True, text=True, timeout=15)

                    # The hook's verified binding works on the first approved
                    # call; no failed tool call or permission probe is needed.
                    first_call = spawn()
                    hook(first_call, 'SessionStart')
                    approved_first = sandbox_title(first_call, allowed=root)
                    assert approved_first.returncode == 0, approved_first.stderr
                    assert json.loads(approved_first.stdout)['status'] == 'renamed'
                    assert name(first_call) == 'Approved sandbox title'

                    access = spawn()
                    hook(access, 'SessionStart')
                    no_runtime = dict(detached_env)
                    no_runtime.pop('XDG_RUNTIME_DIR', None)
                    no_runtime['TMUX_AGENT_WORKBENCH_TMUX_SOCKET'] = socket
                    for run_env in (detached_env, no_runtime):
                        denied = sandbox_title(access, run_env=run_env)
                        assert denied.returncode != 0
                        assert 'socket access denied' in denied.stderr, denied.stderr
                        assert 'permission mechanism' in denied.stderr, denied.stderr
                        assert 'cannot uniquely verify' not in denied.stderr
                        assert name(access) == 'zsh'
                    daemon_only = sandbox_title(access, allowed=root+'/tmux-agent-workbench')
                    assert 'socket access denied' in daemon_only.stderr, daemon_only.stderr
                    assert name(access) == 'zsh'
                    approved = sandbox_title(access, allowed=root)
                    assert approved.returncode == 0, approved.stderr
                    assert json.loads(approved.stdout)['status'] == 'renamed'
                    assert name(access) == 'Approved sandbox title'
                    print('PASS real Codex sandbox: first approved call, denied sockets and scoped approval')

            # Auxiliary sessions can report hooks from the same CLI PID while
            # the visible TUI remains on its original thread. Previously this
            # replaced the owner and invalidated the command already issued.
            foreground = spawn()
            hook(foreground, 'SessionStart')
            hook(foreground, 'UserPromptSubmit')
            thread_title(foreground)
            assert hook(foreground, 'Stop')['decision'] == 'block'
            auxiliary = dict(foreground, session_id=str(uuid.uuid4()))
            for event in ('SessionStart', 'UserPromptSubmit', 'Stop'):
                assert hook(auxiliary, event) == {}, 'auxiliary session must not claim a title or continue'
            assert title(foreground)['repair_used'] is True, 'auxiliary hooks must not reset the repair budget'
            result = title(foreground, 'Node Package Managers')
            assert result['status'] == 'renamed', result
            assert name(foreground) == 'Node Package Managers'

            legacy = spawn()
            hook(legacy, 'SessionStart')
            hook(legacy, 'UserPromptSubmit')
            thread_title(legacy)
            auxiliary = dict(legacy, session_id=str(uuid.uuid4()))
            hook(auxiliary, 'SessionStart')
            legacy_state = json.loads(tmux('show-option', '-wqv', '-t', legacy['pane'], '@workbench_task_title'))
            legacy_state['owner']['session'] = auxiliary['session_id']
            legacy_state['repair_used'] = True
            tmux('set-option', '-w', '-t', legacy['pane'], '@workbench_task_title', json.dumps(legacy_state))
            assert title(legacy, 'Recover issued naming binding')['status'] == 'renamed'

            no_title = spawn()
            hook(no_title, 'SessionStart')
            auxiliary = dict(no_title, session_id=str(uuid.uuid4()))
            assert hook(auxiliary, 'SessionStart') == {}
            assert title(no_title, 'Keep issued binding without OSC')['status'] == 'renamed'

            switched = spawn()
            hook(switched, 'SessionStart')
            hook(switched, 'UserPromptSubmit')
            next_thread = dict(switched, session_id=str(uuid.uuid4()))
            thread_title(next_thread)
            assert hook(next_thread, 'SessionStart', source='clear')['hookSpecificOutput']
            stale = core('title', 'set', 'Stale session title', '--pane', switched['pane'],
                         '--session-id', switched['session_id'], run_env=detached_env, check=False)
            assert stale.returncode != 0 and 'foreground task' in stale.stderr
            assert name(next_thread) == 'zsh'
            assert title(next_thread, 'Actual foreground task')['status'] == 'renamed'

            primary = spawn()
            secondary = spawn(split=primary['pane'])
            assert hook(secondary, 'SessionStart') == {}, 'first hook must not steal an older agent window'
            assert title(secondary)['status'] == 'not_owner'
            hook(primary, 'SessionStart')
            assert hook(primary, 'UserPromptSubmit')['hookSpecificOutput']
            assert title(secondary, 'Steal primary task')['status'] == 'not_owner'
            assert name(primary) == 'zsh'
            stale_env = dict(env, TMUX_PANE=inspection)
            stale = hook(primary, 'UserPromptSubmit', run_env=stale_env)
            assert primary['pane'] in stale['hookSpecificOutput']['additionalContext']
            tmux('select-pane', '-t', inspection)
            assert title(primary, '光标复现与验收')['status'] == 'renamed'
            assert name(primary) == '光标复现与验收'
            bare_locale = dict(detached_env, LC_ALL='C')
            bare_locale.pop('LANG', None)
            assert title(primary, run_env=bare_locale)['title'] == '光标复现与验收'
            assert tmux('display-message', '-p', '-t', inspection, '#{window_name}') == 'User inspection'
            assert title(primary, 'A different task')['status'] == 'preserved'
            assert hook(primary, 'Stop') == {} and hook(secondary, 'Stop') == {}
            tmux('rename-window', '-t', primary['pane'], 'Human renamed task')
            assert title(primary, 'Overwrite human title')['status'] == 'preserved'
            assert name(primary) == 'Human renamed task'

            # A manual edit made after the naming request remains authoritative,
            # even if the human deliberately chooses another generic name.
            edited = spawn()
            hook(edited, 'SessionStart')
            hook(edited, 'UserPromptSubmit')
            tmux('rename-window', '-t', edited['pane'], 'bash')
            assert title(edited, 'Overwrite manual default')['status'] == 'preserved'
            assert hook(edited, 'Stop') == {}

            quoted = spawn()
            hook(quoted, 'SessionStart')
            punctuation = "Fix O'Reilly's $PATH; parsing"
            assert title(quoted, punctuation)['status'] == 'renamed'
            assert name(quoted) == punctuation

            manual = spawn('Keep this task')
            for event in ('SessionStart', 'UserPromptSubmit', 'Stop'):
                assert hook(manual, event) == {}
            assert title(manual, 'Replace named task')['status'] == 'preserved'

            managed = spawn()
            tmux('set-option', '-w', '-t', managed['pane'], '@workbench_project_root', root)
            hook(managed, 'SessionStart')
            assert hook(managed, 'UserPromptSubmit') == {}
            assert title(managed, 'Rename inspection')['status'] == 'preserved'
            tmux('set-option', '-wu', '-t', managed['pane'], '@workbench_project_root')
            tmux('set-option', '-t', 'titles', '@workbench_task', '1')
            assert hook(managed, 'UserPromptSubmit') == {}
            assert title(managed, 'Rename managed task')['status'] == 'preserved'
            tmux('set-option', '-u', '-t', 'titles', '@workbench_task')

            tmux('set-option', '-g', '@workbench-window-automatic-rename', 'on')
            assert hook(managed, 'UserPromptSubmit') == {}
            assert title(managed, 'Override automatic policy')['status'] == 'preserved'
            tmux('set-option', '-gu', '@workbench-window-automatic-rename')

            claude_agent = spawn(kind='claude')
            assert hook(claude_agent, 'SessionStart')['hookSpecificOutput']
            assert hook(claude_agent, 'UserPromptSubmit')['hookSpecificOutput']
            assert hook(claude_agent, 'Stop')['decision'] == 'block'
            assert title(claude_agent, 'Claude task title')['status'] == 'renamed'

            failed = core('title', 'set', 'Wrong target', '--pane', '%99999', '--session-id', repair['session_id'],
                          run_env=detached_env, check=False)
            assert failed.returncode != 0
            assert name(repair) == 'zsh'

        # The tmux window, rather than the daemon process or hook payload, owns
        # the continuation budget. Resume and compaction cannot replenish it.
        with running_daemon(binary, env):
            previous_pid = repair['agent_pid']
            tmux('respawn-pane', '-k', '-t', repair['pane'],
                 'env', 'WORKBENCH_FIXTURE_SECONDS=300', fixture)
            repair['agent_pid'] = wait_agent(repair['pane'], previous_pid)['process']['pid']
            for source in ('resume', 'compact'):
                assert hook(repair, 'SessionStart', source=source)['hookSpecificOutput']
                assert hook(repair, 'Stop') == {}
            assert title(repair)['repair_used'] is True
            new_task = dict(repair, session_id=str(uuid.uuid4()))
            clear = hook(new_task, 'SessionStart', source='clear')
            submitted = hook(new_task, 'UserPromptSubmit')
            stopped = hook(new_task, 'Stop')
            assert stopped.get('decision') == 'block', (
                clear, submitted, stopped,
                tmux('show-option', '-wqv', '-t', repair['pane'], '@workbench_task_title'),
                json.loads(core('snapshot', '--json').stdout))
            assert title(new_task, 'Resumed naming repair')['status'] == 'renamed'
            assert name(new_task) == 'Resumed naming repair'

    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], capture_output=True)

print('task titles: ownership, bounded repairs, resume, manual names and detached hooks passed')
