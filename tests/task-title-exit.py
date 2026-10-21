"""Restore a task title after the real CLI exits, with its shell left alive."""
import fcntl
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon


binary, fixture = [str(Path(arg).resolve()) for arg in sys.argv[1:]]
shell_prompt = '__workbench_title_test_ready__'

with tempfile.TemporaryDirectory(prefix='workbench-title-exit-', dir='/tmp') as root:
    socket = root+'/tmux.sock'
    env = dict(os.environ, TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               XDG_CONFIG_HOME=root+'/config', XDG_STATE_HOME=root+'/state',
               XDG_CACHE_HOME=root+'/cache', XDG_DATA_HOME=root+'/data',
               XDG_RUNTIME_DIR=root, TMUX=socket+',0,0')
    detached = {key: value for key, value in env.items() if key not in ('TMUX', 'TMUX_PANE')}

    def tmux(*args):
        return subprocess.check_output(['tmux', '-u', '-S', socket, *args],
                                       env=env, text=True).rstrip('\n')

    def core(*args, data=None):
        result = subprocess.run([binary, *args], env=detached,
                                input=json.dumps(data) if data is not None else None,
                                capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, (args, result.stderr)
        return json.loads(result.stdout)

    def option(agent, key='@workbench_task_title'):
        return tmux('show-option', '-wqv', '-t', agent['window'], key)

    def window_name(agent):
        return tmux('display-message', '-p', '-t', agent['window'], '#{window_name}')

    def wait_for(check, message):
        deadline = time.monotonic()+20
        while not check():
            assert time.monotonic() < deadline, message() if callable(message) else message
            time.sleep(.05)

    def launch(pane, seconds=300, previous_pid=None, session_id=None, start_hook=True):
        def details(message):
            return (message, pane,
                    tmux('display-message', '-p', '-t', pane,
                         '#{pane_pid}:#{pane_current_command}:#{pane_dead}'),
                    tmux('capture-pane', '-p', '-t', pane),
                    [agent for agent in core('snapshot', '--json')['agents']
                     if agent['target']['pane_id'] == pane])

        # Interactive shells can flush input while setting up their terminal.
        # Wait for the real prompt before sending a new or resumed CLI command.
        wait_for(lambda: tmux('capture-pane', '-p', '-t', pane).rstrip().endswith(shell_prompt),
                 lambda: details('shell was not ready'))
        command = 'env WORKBENCH_FIXTURE_SECONDS='+str(seconds)+' '+shlex.quote(fixture)
        tmux('send-keys', '-l', '-t', pane, command)
        tmux('send-keys', '-t', pane, 'Enter')
        found = []

        def discovered():
            found[:] = [agent for agent in core('snapshot', '--json')['agents']
                        if agent['target']['pane_id'] == pane and not agent['exited']
                        and agent['process'] and agent['process']['pid'] != previous_pid]
            return len(found) == 1

        wait_for(discovered, lambda: details('CLI was not discovered'))
        agent = {'pane': pane, 'window': found[0]['target']['window_id'],
                 'pid': found[0]['process']['pid'],
                 'session_id': session_id or str(uuid.uuid4())}
        if start_hook:
            hook(agent, 'SessionStart')
        return agent

    def spawn(seconds=300, split=None, start_hook=True):
        args = ['split-window', '-d', '-t', split] if split else ['new-window', '-d', '-n', 'zsh']
        pane = tmux(*args, '-P', '-F', '#{pane_id}',
                    'env', 'PS1='+shell_prompt+' ', '/bin/sh')
        return launch(pane, seconds, start_hook=start_hook)

    def hook(agent, event):
        return core('hook', 'ingest', 'codex', event,
                    data={'session_id': agent['session_id'], 'agent_pid': agent['pid'], 'cwd': root})

    def title(agent, text):
        result = core('title', 'set', '--pane', agent['pane'],
                      '--session-id', agent['session_id'], text)
        assert result['status'] == 'renamed', result

    def restored(agent, name='zsh', automatic='off'):
        wait_for(lambda: window_name(agent) == name and not option(agent)
                 and option(agent, 'automatic-rename') == automatic,
                 'title/options were not restored for '+agent['window'])

    def lock_file(agent):
        files = list(Path(root, 'tmux-agent-workbench').glob(
            'task-title-*-'+agent['window'][1:]+'.lock'))
        assert len(files) == 1, files
        return files[0]

    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'exit-tests',
             '-n', 'User inspection', 'sleep', '300')
        tmux('set-option', '-g', 'automatic-rename', 'off')
        with running_daemon(binary, env):
            natural = spawn(seconds=10)
            tmux('set-option', '-wu', '-t', natural['window'], 'automatic-rename')
            hook(natural, 'UserPromptSubmit')
            semantic = "CU, 功能} # O'Reilly's $PATH; 测试"
            title(natural, semantic)
            hook(natural, 'Stop')
            time.sleep(1.1)
            assert window_name(natural) == semantic, 'a completed turn is not a CLI exit'
            restored(natural, automatic='')
            assert tmux('display-message', '-p', '-t', natural['pane'], '#{pane_dead}') == '0'
            print('PASS natural CLI exit restores Unicode/punctuation title and inherited options')

            reused = launch(natural['pane'], previous_pid=natural['pid'])
            title(reused, 'A second task')
            os.kill(reused['pid'], signal.SIGKILL)
            restored(reused, automatic='')
            print('PASS forced exit restores the previous title in a reused shell pane')

            local = spawn()
            title(local, 'Explicit window options')
            os.kill(local['pid'], signal.SIGTERM)
            restored(local)
            print('PASS explicit automatic-rename off is restored')

            automatic = spawn(start_hook=False)
            tmux('set-option', '-w', '-t', automatic['window'], 'automatic-rename', 'on')
            wait_for(lambda: window_name(automatic) == 'codex', 'automatic title did not settle')
            hook(automatic, 'SessionStart')
            title(automatic, 'Restore automatic naming')
            os.kill(automatic['pid'], signal.SIGKILL)
            wait_for(lambda: not option(automatic) and option(automatic, 'automatic-rename') == 'on'
                     and window_name(automatic) == tmux('display-message', '-p', '-t', automatic['pane'],
                                                        '#{pane_current_command}'),
                     lambda: ('automatic naming did not resume at the shell', window_name(automatic),
                              option(automatic), option(automatic, 'automatic-rename'),
                              tmux('display-message', '-p', '-t', automatic['pane'], '#{pane_current_command}')))
            print('PASS previous automatic-rename on resumes shell naming after exit')

            changed_options = spawn()
            title(changed_options, 'Keep manual options')
            tmux('set-option', '-wu', '-t', changed_options['window'], 'automatic-rename')
            os.kill(changed_options['pid'], signal.SIGKILL)
            restored(changed_options, automatic='')
            print('PASS an automatic-rename change after naming is preserved')

            human = spawn()
            title(human, 'Temporary agent name')
            with lock_file(human).open('r+') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                os.kill(human['pid'], signal.SIGKILL)
                time.sleep(1.1)
                assert window_name(human) == 'Temporary agent name'
                tmux('rename-window', '-t', human['window'], 'Human keeps this')
            restored(human, 'Human keeps this')
            print('PASS a busy cleanup lock retries and a manual title wins')

            owner = spawn()
            title(owner, 'Shared window owner')
            secondary = spawn(split=owner['pane'])
            os.kill(secondary['pid'], signal.SIGKILL)
            time.sleep(1.1)
            assert window_name(owner) == 'Shared window owner'
            os.kill(owner['pid'], signal.SIGKILL)
            restored(owner)
            print('PASS only the naming owner exit restores a shared window')

            removed = spawn()
            title(removed, 'Owner pane closes')
            tmux('split-window', '-d', '-t', removed['pane'], 'sleep', '300')
            tmux('kill-pane', '-t', removed['pane'])
            restored(removed)
            print('PASS closing the owner pane restores its surviving window')

            resumed = spawn()
            title(resumed, 'Resume the same task')
            with lock_file(resumed).open('r+') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                os.kill(resumed['pid'], signal.SIGKILL)
                replacement = launch(resumed['pane'], previous_pid=resumed['pid'],
                                     session_id=resumed['session_id'])
            wait_for(lambda: json.loads(option(replacement))['owner']['process']['pid'] == replacement['pid'],
                     'resumed process did not inherit the title')
            assert window_name(replacement) == 'Resume the same task'
            os.kill(replacement['pid'], signal.SIGKILL)
            restored(replacement)
            print('PASS verified in-place resume keeps the lease until its final exit')

            offline = spawn()
            title(offline, 'Daemon was offline')

        os.kill(offline['pid'], signal.SIGKILL)
        assert window_name(offline) == 'Daemon was offline'
        assert option(offline)
        with running_daemon(binary, env):
            restored(offline)
            print('PASS restart recovers exit cleanup from durable tmux state')
    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

print('task title exit restoration: ok')
