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
               TMUX=socket+',0,0', CODEX_HOME=root+'/codex')
    detached_env = {key: value for key, value in env.items()
                    if key not in ('TMUX', 'TMUX_PANE', 'TMUX_AGENT_WORKBENCH_TMUX_SOCKET')}
    config_dir = Path(env['XDG_CONFIG_HOME'], 'tmux-agent-workbench')
    config_dir.mkdir(parents=True)
    # This suite verifies naming. Desktop delivery has separate coverage and
    # must not open notification overlays on the runner while hooks are tested.
    (config_dir / 'config.toml').write_text('[notifications]\nenabled = false\n')
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

    def wait_hook_context(agent, event, run_env=None, **extra):
        # Daemons discover CLIs and associate their hook sessions asynchronously.
        # A busy hook may safely return {}. Require the real context once the
        # binding is ready, while keeping the one-shot Stop assertions below.
        deadline = time.monotonic() + 8
        while True:
            output = hook(agent, event, run_env=run_env, **extra)
            specific = output.get('hookSpecificOutput')
            if specific:
                assert specific['hookEventName'] == event, output
                context = specific['additionalContext']
                assert agent['pane'] in context and agent['session_id'] in context, output
                return output
            assert output == {}, output
            if time.monotonic() >= deadline:
                raise AssertionError(('naming context did not become ready', event, extra,
                                      agent, output,
                                      tmux('show-option', '-wqv', '-t', agent['pane'], '@workbench_task_title'),
                                      json.loads(core('snapshot', '--json').stdout)))
            time.sleep(.05)

    def wait_hook_binding(agent):
        # Hooks can be spooled after a bounded transport timeout. Observe the
        # accepted session before asserting ownership after intentional no-op
        # hooks (manual names, managed windows, and auxiliary sessions).
        deadline = time.monotonic() + 8
        while True:
            snapshot = json.loads(core('snapshot', '--json').stdout)
            if any(item['target']['pane_id'] == agent['pane']
                   and not item['exited']
                   and (item.get('process') or {}).get('pid') == agent['agent_pid']
                   and item.get('hook_session_id') == agent['session_id']
                   for item in snapshot['agents']):
                return
            if time.monotonic() >= deadline:
                raise AssertionError(('hook session did not become ready', agent, snapshot))
            time.sleep(.05)

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

    def wait_name(agent, expected):
        # Hooks may spool accepted reports while the daemon is busy. Observe
        # the eventual title instead of treating the hook's reply as a redraw.
        deadline = time.monotonic() + 8
        while True:
            actual = name(agent)
            if actual == expected:
                return
            if time.monotonic() >= deadline:
                raise AssertionError(('thread title did not sync', expected, actual,
                                      agent, json.loads(core('snapshot', '--json').stdout)))
            time.sleep(.05)

    def thread_title(agent):
        tty = tmux('display-message', '-p', '-t', agent['pane'], '#{pane_tty}')
        with open(tty, 'w') as stream:
            stream.write('\x1b]2;codex | Ready | ' + agent['session_id'][:32] + '...\x07')
            stream.flush()
        deadline = time.monotonic() + 3
        while agent['session_id'][:32] not in tmux('display-message', '-p', '-t', agent['pane'], '#{pane_title}'):
            assert time.monotonic() < deadline
            time.sleep(.02)

    def wait_visible_session(agent):
        # A pane redraw precedes the daemon's title scan. Wait for its durable
        # identity update before testing a one-shot hook's repair decision;
        # a hook intentionally returns {} while that naming lock is held.
        deadline = time.monotonic() + 8
        while True:
            state = json.loads(tmux('show-option', '-wqv', '-t', agent['pane'], '@workbench_task_title'))
            visible = state.get('visible_session')
            if visible and agent['session_id'].startswith(visible):
                return
            assert time.monotonic() < deadline, ('visible session did not settle', agent, state)
            time.sleep(.05)

    try:
        inspection = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'titles',
                          '-n', 'User inspection', '-P', '-F', '#{pane_id}', 'sleep', '300')
        tmux('set-option', '-g', 'automatic-rename', 'off')
        with running_daemon(binary, env):
            repair = spawn()
            startup = wait_hook_context(repair, 'SessionStart')
            assert startup.get('hookSpecificOutput', {}).get('hookEventName') == 'SessionStart', (
                startup, json.loads(core('snapshot', '--json').stdout))
            assert hook(repair, 'Stop') == {}, 'startup without a user task must not continue'
            context = wait_hook_context(repair, 'UserPromptSubmit')['hookSpecificOutput']['additionalContext']
            assert repair['pane'] in context and repair['session_id'] in context
            stripped_env = dict(env)
            for key in ('TMUX', 'TMUX_PANE', 'XDG_RUNTIME_DIR'):
                stripped_env.pop(key, None)
            stripped = wait_hook_context(repair, 'UserPromptSubmit', run_env=stripped_env)
            assert repair['pane'] in stripped['hookSpecificOutput']['additionalContext']
            recovered = core('title', 'status', '--pane', repair['pane'], '--session-id', repair['session_id'],
                             run_env=stripped_env)
            assert json.loads(recovered.stdout)['status'] == 'pending'

            # An earlier tool runner can have started a second daemon after
            # dropping XDG_RUNTIME_DIR. Both daemons observe the same CLI and
            # durable title binding; that is one caller, not two owners.
            legacy_env = dict(env, XDG_RUNTIME_DIR=root+'/legacy', XDG_STATE_HOME=root+'/legacy-state')
            with running_daemon(binary, legacy_env):
                deadline = time.monotonic() + 5
                while True:
                    legacy = json.loads(core('snapshot', '--json', run_env=legacy_env).stdout)
                    if any(agent['target']['pane_id'] == repair['pane'] and not agent['exited']
                           for agent in legacy['agents']):
                        break
                    assert time.monotonic() < deadline, 'legacy daemon did not observe the caller'
                    time.sleep(.05)
                # The parent retains the old runtime while the tool's own
                # environment is stripped, matching shared execution backends.
                duplicate = subprocess.run(
                    [sys.executable, '-c',
                     'import os, subprocess, sys; env=dict(os.environ); '
                     'env.pop("XDG_RUNTIME_DIR", None); env.pop("TMUX", None); '
                     'sys.exit(subprocess.run(sys.argv[1:], env=env).returncode)',
                     binary, 'title', 'status', '--pane', repair['pane'],
                     '--session-id', repair['session_id']],
                    env=legacy_env, text=True, capture_output=True, timeout=15)
                assert duplicate.returncode == 0, duplicate.stderr
                assert json.loads(duplicate.stdout)['status'] == 'pending'

                # Ordinary calls with a stripped runtime must use the same
                # daemon as tmux, instead of creating another runtime socket.
                primary_pid = json.loads(core('daemon', 'status').stdout)['pid']
                recovered_pid = json.loads(core('daemon', 'status', run_env=stripped_env).stdout)['pid']
                assert recovered_pid == primary_pid
                core('daemon', 'ensure', run_env=stripped_env)
                assert json.loads(core('daemon', 'status', run_env=stripped_env).stdout)['pid'] == primary_pid
                assert json.loads(core('daemon', 'status', run_env=legacy_env).stdout)['pid'] != primary_pid
            print('PASS stripped runtime reuses tmux daemon; duplicate daemons remain one verified caller')
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
                # The installed CLI may differ from the user's configuration
                # schema. Sandbox verification must not load personal settings.
                codex_config = Path(root, 'codex-config')
                codex_config.mkdir()
                sandbox_env = dict(detached_env, CODEX_HOME=str(codex_config))
                sandbox_help = subprocess.run(['codex', 'sandbox', '--help'],
                                              env=sandbox_env,
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
                                              env=dict(run_env or detached_env,
                                                       CODEX_HOME=str(codex_config)),
                                              capture_output=True, text=True, timeout=15)

                    # The hook's verified binding works on the first approved
                    # call; no failed tool call or permission probe is needed.
                    first_call = spawn()
                    wait_hook_context(first_call, 'SessionStart')
                    approved_first = sandbox_title(first_call, allowed=root)
                    assert approved_first.returncode == 0, approved_first.stderr
                    assert json.loads(approved_first.stdout)['status'] == 'renamed'
                    assert name(first_call) == 'Approved sandbox title'

                    access = spawn()
                    wait_hook_context(access, 'SessionStart')
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
            wait_hook_context(foreground, 'SessionStart')
            wait_hook_context(foreground, 'UserPromptSubmit')
            thread_title(foreground)
            assert hook(foreground, 'Stop')['decision'] == 'block'
            auxiliary = dict(foreground, session_id=str(uuid.uuid4()))
            for event in ('SessionStart', 'UserPromptSubmit', 'Stop'):
                assert hook(auxiliary, event) == {}, 'auxiliary session must not claim a title or continue'
            wait_hook_binding(auxiliary)
            assert title(foreground)['repair_used'] is True, 'auxiliary hooks must not reset the repair budget'
            result = title(foreground, 'Node Package Managers')
            assert result['status'] == 'renamed', result
            assert name(foreground) == 'Node Package Managers'

            legacy = spawn()
            wait_hook_context(legacy, 'SessionStart')
            wait_hook_context(legacy, 'UserPromptSubmit')
            thread_title(legacy)
            auxiliary = dict(legacy, session_id=str(uuid.uuid4()))
            hook(auxiliary, 'SessionStart')
            wait_hook_binding(auxiliary)
            legacy_state = json.loads(tmux('show-option', '-wqv', '-t', legacy['pane'], '@workbench_task_title'))
            legacy_state['owner']['session'] = auxiliary['session_id']
            legacy_state['repair_used'] = True
            tmux('set-option', '-w', '-t', legacy['pane'], '@workbench_task_title', json.dumps(legacy_state))
            assert title(legacy, 'Recover issued naming binding')['status'] == 'renamed'

            no_title = spawn()
            wait_hook_context(no_title, 'SessionStart')
            auxiliary = dict(no_title, session_id=str(uuid.uuid4()))
            assert hook(auxiliary, 'SessionStart') == {}
            wait_hook_binding(auxiliary)
            assert title(no_title, 'Keep issued binding without OSC')['status'] == 'renamed'

            switched = spawn()
            wait_hook_context(switched, 'SessionStart')
            wait_hook_context(switched, 'UserPromptSubmit')
            next_thread = dict(switched, session_id=str(uuid.uuid4()))
            thread_title(next_thread)
            wait_hook_context(next_thread, 'SessionStart', source='clear')
            stale = core('title', 'set', 'Stale session title', '--pane', switched['pane'],
                         '--session-id', switched['session_id'], run_env=detached_env, check=False)
            assert stale.returncode != 0 and 'foreground task' in stale.stderr
            assert name(next_thread) == 'zsh'
            assert title(next_thread, 'Actual foreground task')['status'] == 'renamed'

            # Codex redraws a cleared thread before emitting SessionStart on its
            # first prompt. Reset immediately, including truncated OSC UUIDs.
            for named_before_clear in (False, True):
                pending = spawn()
                if named_before_clear:
                    hook(pending, 'SessionStart', thread_name='Previous pending thread')
                    wait_name(pending, 'Previous pending thread')
                else:
                    wait_hook_context(pending, 'SessionStart')
                    assert title(pending, 'Previous pending task')['status'] == 'renamed'
                fresh_pending = dict(pending, session_id=str(uuid.uuid4()))
                thread_title(fresh_pending)
                wait_name(fresh_pending, 'zsh')
                assert wait_agent(pending['pane'])['hook_session_id'] == pending['session_id']
                pending_state = json.loads(tmux('show-option', '-wqv', '-t', pending['pane'], '@workbench_task_title'))
                assert fresh_pending['session_id'].startswith(pending_state['owner']['session'])
                wait_hook_context(fresh_pending, 'SessionStart', source='clear')
                assert title(fresh_pending, 'New pending task')['status'] == 'renamed'
                assert name(fresh_pending) == 'New pending task'

            # Clear starts a new task in the same CLI process. The previous
            # thread's OSC title can still be visible while its hook runs.
            cleared = spawn()
            wait_hook_context(cleared, 'SessionStart')
            wait_hook_context(cleared, 'UserPromptSubmit')
            assert hook(cleared, 'Stop')['decision'] == 'block'
            tmux('set-option', '-wu', '-t', cleared['pane'], 'automatic-rename')
            assert title(cleared, 'Previous task title')['status'] == 'renamed'
            thread_title(cleared)
            fresh = dict(cleared, session_id=str(uuid.uuid4()))
            wait_hook_context(fresh, 'SessionStart', source='clear')
            assert name(fresh) == 'zsh'
            assert tmux('show-option', '-wqv', '-t', fresh['pane'], 'automatic-rename') == ''
            fresh_state = json.loads(tmux('show-option', '-wqv', '-t', fresh['pane'], '@workbench_task_title'))
            assert fresh_state['owner']['session'] == fresh['session_id']
            assert fresh_state['title'] is None and fresh_state['repair_used'] is False
            assert hook(fresh, 'Stop') == {}, 'clear alone must not request a naming continuation'
            thread_title(fresh)
            wait_visible_session(fresh)
            wait_hook_context(fresh, 'UserPromptSubmit')
            stopped = hook(fresh, 'Stop')
            assert stopped.get('decision') == 'block', (stopped, title(fresh))
            assert title(fresh, 'Fresh task title')['status'] == 'renamed'
            assert hook(fresh, 'SessionStart', source='clear') == {}
            assert name(fresh) == 'Fresh task title', 'a duplicate clear must not erase the new title'

            # Conversely, a hook may precede the OSC redraw. Its new named
            # thread must not be reset back to the still-visible old UUID.
            before_redraw = spawn()
            wait_hook_context(before_redraw, 'SessionStart')
            thread_title(before_redraw)
            assert title(before_redraw, 'Before redraw')['status'] == 'renamed'
            after_redraw = dict(before_redraw, session_id=str(uuid.uuid4()))
            hook(after_redraw, 'SessionStart', source='clear', thread_name='After redraw')
            wait_name(after_redraw, 'After redraw')
            time.sleep(1.1)  # Include multiple daemon title scans before the redraw.
            assert name(after_redraw) == 'After redraw'
            thread_title(after_redraw)
            wait_visible_session(after_redraw)
            wait_name(after_redraw, 'After redraw')

            automatic_clear = spawn()
            tmux('set-option', '-w', '-t', automatic_clear['pane'], 'automatic-rename', 'on')
            wait_name(automatic_clear, 'codex')
            wait_hook_context(automatic_clear, 'SessionStart')
            assert title(automatic_clear, 'Temporarily named task')['status'] == 'renamed'
            automatic_fresh = dict(automatic_clear, session_id=str(uuid.uuid4()))
            wait_hook_context(automatic_fresh, 'SessionStart', source='clear')
            wait_name(automatic_fresh, 'codex')
            assert tmux('show-option', '-wqv', '-t', automatic_fresh['pane'], 'automatic-rename') == 'on'

            named_clear = spawn()
            hook(named_clear, 'SessionStart', thread_name='Previous thread name')
            wait_name(named_clear, 'Previous thread name')
            unnamed = dict(named_clear, session_id=str(uuid.uuid4()))
            wait_hook_context(unnamed, 'SessionStart', source='clear')
            assert name(unnamed) == 'zsh'
            renamed = dict(unnamed, session_id=str(uuid.uuid4()))
            assert hook(renamed, 'SessionStart', source='clear', thread_name='New thread name') == {}
            wait_name(renamed, 'New thread name')
            assert json.loads(tmux('show-option', '-wqv', '-t', renamed['pane'], '@workbench_task_title'))['initial_name'] == 'zsh'

            human_clear = spawn()
            wait_hook_context(human_clear, 'SessionStart')
            assert title(human_clear, 'Agent title before manual edit')['status'] == 'renamed'
            tmux('rename-window', '-t', human_clear['pane'], 'Human keeps this title')
            human_fresh = dict(human_clear, session_id=str(uuid.uuid4()))
            assert hook(human_fresh, 'SessionStart', source='clear') == {}
            wait_hook_binding(human_fresh)
            assert name(human_fresh) == 'Human keeps this title'
            print('PASS clear resets owned titles, options and repair budgets while preserving manual names')

            primary = spawn()
            secondary = spawn(split=primary['pane'])
            assert hook(secondary, 'SessionStart') == {}, 'first hook must not steal an older agent window'
            wait_hook_binding(secondary)
            assert title(secondary)['status'] == 'not_owner'
            wait_hook_context(primary, 'SessionStart')
            wait_hook_context(primary, 'UserPromptSubmit')
            assert title(secondary, 'Steal primary task')['status'] == 'not_owner'
            assert name(primary) == 'zsh'
            stale_env = dict(env, TMUX_PANE=inspection)
            stale = wait_hook_context(primary, 'UserPromptSubmit', run_env=stale_env)
            assert primary['pane'] in stale['hookSpecificOutput']['additionalContext']
            tmux('select-pane', '-t', inspection)
            assert title(primary, '光标复现与验收')['status'] == 'renamed'
            assert name(primary) == '光标复现与验收'
            secondary = dict(secondary, session_id=str(uuid.uuid4()))
            assert hook(secondary, 'SessionStart', source='clear') == {}
            assert name(primary) == '光标复现与验收', 'clearing another pane must preserve the owner title'
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
            wait_hook_context(edited, 'SessionStart')
            wait_hook_context(edited, 'UserPromptSubmit')
            tmux('rename-window', '-t', edited['pane'], 'bash')
            assert title(edited, 'Overwrite manual default')['status'] == 'preserved'
            assert hook(edited, 'Stop') == {}

            quoted = spawn()
            wait_hook_context(quoted, 'SessionStart')
            punctuation = "Fix O'Reilly's $PATH; parsing"
            assert title(quoted, punctuation)['status'] == 'renamed'
            assert name(quoted) == punctuation

            manual = spawn('Keep this task')
            for event in ('SessionStart', 'UserPromptSubmit', 'Stop'):
                assert hook(manual, event) == {}
            wait_hook_binding(manual)
            assert title(manual, 'Replace named task')['status'] == 'preserved'

            managed = spawn()
            tmux('set-option', '-w', '-t', managed['pane'], '@workbench_project_root', root)
            hook(managed, 'SessionStart')
            wait_hook_binding(managed)
            assert hook(managed, 'UserPromptSubmit') == {}
            assert title(managed, 'Rename inspection')['status'] == 'preserved'
            managed = dict(managed, session_id=str(uuid.uuid4()))
            assert hook(managed, 'SessionStart', source='clear') == {}
            assert name(managed) == 'zsh'
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
            wait_hook_context(claude_agent, 'SessionStart')
            wait_hook_context(claude_agent, 'UserPromptSubmit')
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
                wait_hook_context(repair, 'SessionStart', source=source)
                assert hook(repair, 'Stop') == {}
            assert title(repair)['repair_used'] is True
            new_task = dict(repair, session_id=str(uuid.uuid4()))
            clear = wait_hook_context(new_task, 'SessionStart', source='clear')
            submitted = wait_hook_context(new_task, 'UserPromptSubmit')
            stopped = hook(new_task, 'Stop')
            assert stopped.get('decision') == 'block', (
                clear, submitted, stopped,
                tmux('show-option', '-wqv', '-t', repair['pane'], '@workbench_task_title'),
                json.loads(core('snapshot', '--json').stdout))
            assert title(new_task, 'Resumed naming repair')['status'] == 'renamed'
            assert name(new_task) == 'Resumed naming repair'

            # Thread-name is shared by the sidebar and automatically owned title.
            named = spawn()
            assert hook(named, 'SessionStart', thread_name='Readable thread') == {}
            wait_name(named, 'Readable thread')
            assert hook(named, 'UserPromptSubmit', thread_name='Renamed thread') == {}
            wait_name(named, 'Renamed thread')
            tracked = wait_agent(named['pane'])
            assert tracked['thread_name'] == 'Renamed thread'
            assert tracked['label'] == 'Renamed thread'
            Path(root, 'codex').mkdir(exist_ok=True)
            Path(root, 'codex/session_index.jsonl').write_text(json.dumps({
                'id': named['session_id'], 'thread_name': 'Renamed from UI'}) + '\n')
            deadline = time.monotonic() + 8
            while name(named) != 'Renamed from UI':
                assert time.monotonic() < deadline
                time.sleep(.1)
            assert wait_agent(named['pane'])['thread_name'] == 'Renamed from UI'
            tty = tmux('display-message', '-p', '-t', named['pane'], '#{pane_tty}')
            with open(tty, 'w') as stream:
                stream.write('\x1b]2;Codex | Working | Renamed thread | 2/5\x07')
                stream.flush()
            deadline = time.monotonic() + 8
            while wait_agent(named['pane']).get('task_progress') != {'completed': 2, 'total': 5}:
                assert time.monotonic() < deadline
                time.sleep(.1)
            tmux('rename-window', '-t', named['pane'], 'Manual override')
            assert hook(named, 'UserPromptSubmit', thread_name='Another topic') == {}
            assert name(named) == 'Manual override'

            task = spawn('Planned task')
            tmux('set-option', '-w', '-t', task['pane'], '@workbench_task_root', root + '/tasks/planned')
            tmux('set-option', '-w', '-t', task['pane'], '@workbench_task_name', 'Planned task')
            assert hook(task, 'SessionStart', thread_name='Actual thread topic') == {}
            wait_name(task, 'Actual thread topic')
            assert hook(task, 'UserPromptSubmit', thread_name='Updated thread topic') == {}
            wait_name(task, 'Updated thread topic')

    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], capture_output=True)

print('task titles: ownership, bounded repairs, resume, manual names and detached hooks passed')
