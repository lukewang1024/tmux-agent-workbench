"""Task isolation and temporary tools against a private, config-free tmux server."""
import os
import fcntl
import pty
import select
import struct
import termios
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest


REPO = Path(__file__).resolve().parents[1]
CLI = REPO / 'bin/tmux-agent-workbench-cli'


class TaskWindows(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='workbench-task-test-')
        self.root = Path(self.temp.name).resolve()
        self.code = self.root / 'code'
        self.workspaces = self.root / 'workspaces'
        self.bin = self.root / 'bin'
        for directory in (self.code, self.workspaces, self.bin):
            directory.mkdir()
        self.socket = str(self.root / 'tmux.sock')
        real_tmux = shutil.which('tmux')
        self.write_script('tmux', f'exec {shlex.quote(real_tmux)} -S {shlex.quote(self.socket)} -f /dev/null "$@"\n')
        self.write_script('agent-team', 'printf "%s\\n" "$PWD" >> "$WORKBENCH_TEST_AGENT_LOG"\nexec sleep 300\n')
        self.write_script('core', 'exit 0\n')
        (self.bin / 'tmux-agent-workbench').symlink_to(CLI)
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(('WORKBENCH_', 'TMUX', 'XDG_'))}
        self.env.update(PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        WORKBENCH_CODE_ROOT=str(self.code),
                        WORKBENCH_WORKSPACE_ROOT=str(self.workspaces),
                        WORKBENCH_TEST_AGENT_LOG=str(self.root / 'agents.log'),
                        TMUX_AGENT_WORKBENCH_BIN=str(self.bin / 'core'),
                        XDG_CONFIG_HOME=str(self.root / 'config'),
                        XDG_DATA_HOME=str(self.root / 'data'),
                        XDG_STATE_HOME=str(self.root / 'state'),
                        XDG_CACHE_HOME=str(self.root / 'cache'))
        self.make_repo('app')
        self.make_repo('api')

    def tearDown(self):
        self.tmux('kill-server', check=False)
        self.temp.cleanup()

    def write_script(self, name, text):
        path = self.bin / name
        path.write_text('#!/bin/sh\nset -eu\n' + text)
        path.chmod(0o755)

    def call(self, argv, env=None, check=True):
        result = subprocess.run([str(a) for a in argv], env=env or self.env,
                                cwd=self.root, text=True, capture_output=True, timeout=25)
        if check and result.returncode:
            self.fail(f'{argv}: {result.returncode}\n{result.stdout}\n{result.stderr}')
        return result

    def tmux(self, *args, check=True):
        return self.call([self.bin / 'tmux', *args], check=check).stdout.strip()

    def cli(self, *args, task=None, pane=None, session='direction', check=True, extra=None):
        env = dict(self.env)
        if session:
            env['WORKBENCH_SESSION'] = session
        if task:
            env['WORKBENCH_TASK'] = task
        if pane:
            env['TMUX_PANE'] = pane
        env.update(extra or {})
        return self.call([CLI, *args], env=env, check=check)

    def git(self, path, *args):
        return self.call(['git', '-C', path, *args]).stdout.strip()

    def make_repo(self, name):
        path = self.code / name
        path.mkdir()
        self.git(path, 'init', '-q', '-b', 'main')
        self.git(path, 'config', 'user.email', 'test@example.com')
        self.git(path, 'config', 'user.name', 'Task Test')
        (path / 'file').write_text('original\n')
        self.git(path, 'add', 'file')
        self.git(path, 'commit', '-qm', 'seed')
        remote = self.root / f'{name}.git'
        self.call(['git', 'clone', '-q', '--bare', path, remote])
        self.git(path, 'remote', 'add', 'origin', str(remote))
        self.git(path, 'fetch', '-q', 'origin')
        self.git(path, 'remote', 'set-head', 'origin', '-a')

    def new(self, task='first', *repos):
        self.cli('new', '--detach', '--task', task, 'direction', *repos, session=None)
        self.wait(lambda: self.root.joinpath('agents.log').exists())

    def task_root(self, task):
        return self.workspaces / 'direction' / 'tasks' / task

    def pane(self, task):
        return self.tmux('list-panes', '-t', f'direction:{task}', '-f', '#{!=:#{@pane_role},task}', '-F', '#{pane_id}').splitlines()[0]

    def wait(self, predicate):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.05)
        self.fail('timed out waiting for task state')

    def windows(self):
        return self.tmux('list-windows', '-t', 'direction', '-F', '#{window_name}').splitlines()

    def test_parallel_tasks_have_separate_checkouts_and_agents(self):
        self.new('first', 'app', 'api:custom-api')
        self.cli('task', 'new', 'second', 'app')
        self.assertEqual(self.windows(), ['first', 'second'])
        for name in ('first', 'second'):
            self.assertEqual(self.tmux('list-panes', '-t', f'direction:{name}', '-F', '#{pane_id}').count('\n'), 0)
            self.assertEqual(self.git(self.task_root(name) / 'app', 'branch', '--show-current'), f'task/direction/{name}')
        self.assertEqual(self.git(self.task_root('first') / 'api', 'branch', '--show-current'), 'custom-api')
        (self.task_root('first') / 'app/file').write_text('changed in first\n')
        self.assertEqual((self.task_root('second') / 'app/file').read_text(), 'original\n')
        self.assertEqual((self.code / 'app/file').read_text(), 'original\n')
        self.assertEqual(self.git(self.code / 'app', 'branch', '--show-current'), 'main')
        original = self.pane('second')
        self.cli('task', 'new', 'second', 'app')
        self.assertEqual(self.pane('second'), original)
        self.assertEqual(self.windows(), ['first', 'second'])
        # Reusing another task's branch must fail instead of sharing its checkout.
        result = self.cli('add', 'api:custom-api', task='second', check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.task_root('second') / 'api').exists())

    def test_commands_target_the_calling_task_and_services_do_not_collide(self):
        self.new('first', 'app')
        self.cli('task', 'new', 'second', 'app')
        first, second = self.pane('first'), self.pane('second')
        self.tmux('select-window', '-t', 'direction:second')
        self.cli('add', 'api', pane=first)
        self.assertTrue((self.task_root('first') / 'api/.git').exists())
        self.assertFalse((self.task_root('second') / 'api').exists())
        self.assertNotEqual(self.cli('add', 'api', check=False).returncode, 0)
        self.assertNotEqual(self.cli('run', str(self.task_root('second') / 'app'), '--', 'sleep', '300', pane=first, check=False).returncode, 0)
        service_one = self.cli('run', '--name', 'dev', str(self.task_root('first') / 'app'), '--', 'sleep', '300', pane=first).stdout.strip()
        service_two = self.cli('run', '--name', 'dev', str(self.task_root('second') / 'app'), '--', 'sleep', '300', pane=second).stdout.strip()
        self.assertNotEqual(service_one, service_two)
        self.assertEqual(self.tmux('display-message', '-p', '-t', service_one, '#{window_name}'), 'first')
        again = self.cli('run', '--name', 'dev', str(self.task_root('first') / 'app'), '--', 'sleep', '300', pane=first).stdout.strip()
        self.assertEqual(service_one, again)
        self.assertEqual(self.tmux('display-message', '-p', '-t', service_two, '#{pane_dead}'), '0')
        self.assertEqual(self.tmux('display-message', '-p', '-t', first, '#{pane_active}'), '1')

    def test_temporary_tool_closes_its_pane_without_creating_a_window(self):
        self.new('first', 'app')
        caller = self.pane('first')
        tool = self.cli('inspect', str(self.task_root('first') / 'app'), '--pane', pane=caller,
                        extra={'WORKBENCH_GIT_TOOL': 'sleep 0.3'}).stdout.strip()
        self.wait(lambda: tool not in self.tmux('list-panes', '-a', '-F', '#{pane_id}').splitlines())
        self.assertEqual(self.windows(), ['first'])
        self.assertEqual(self.tmux('list-panes', '-t', 'direction:first', '-F', '#{pane_id}'), caller)
        result = self.cli('inspect', str(self.code / 'app'), '--pane', pane=caller, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.windows(), ['first'])

    def test_task_cleanup_keeps_dirty_work_and_other_tasks(self):
        self.new('first', 'app', 'api')
        self.cli('task', 'new', 'second', 'app')
        dirty = self.task_root('first') / 'api/file'
        dirty.write_text('unfinished\n')
        self.assertNotEqual(self.cli('task', 'done', 'first', check=False).returncode, 0)
        self.assertTrue((self.task_root('first') / 'app/.git').exists())
        self.assertEqual(self.windows(), ['first', 'second'])
        self.cli('task', 'done', '--force', 'first')
        self.assertEqual(self.windows(), ['second'])
        self.assertFalse(self.task_root('first').exists())
        self.assertTrue((self.task_root('second') / 'app/.git').exists())
        scratch = self.task_root('second') / 'notes.txt'
        scratch.write_text('keep me')
        self.assertNotEqual(self.cli('task', 'done', '--force', 'second', check=False).returncode, 0)
        self.assertEqual(scratch.read_text(), 'keep me')
        self.assertTrue((self.task_root('second') / 'app/.git').exists())

    def test_generator_restores_only_task_windows(self):
        self.new('first', 'app', 'api')
        self.cli('task', 'new', 'second', 'app')
        config = self.root / 'config/tmuxinator/direction.yml'
        text = config.read_text()
        self.assertEqual(re.findall(r"^  - '([^']+)':$", text, re.M), ['first', 'second'])
        self.assertNotIn('layout:', text)
        self.assertNotIn('even-vertical', text)
        commands = [line.strip()[2:][1:-1].replace("''", "'")
                    for line in text.splitlines() if line.startswith('        - ')]
        self.tmux('kill-session', '-t', 'direction')
        for index, (name, command) in enumerate(zip(('first', 'second'), commands)):
            if index == 0:
                self.tmux('new-session', '-d', '-s', 'direction', '-n', name,
                          '-c', str(self.workspaces / 'direction'), command)
            else:
                self.tmux('new-window', '-d', '-t', 'direction:', '-n', name, command)
        self.wait(lambda: all(self.tmux('show-option', '-wqv', '-t', f'direction:{name}', '@workbench_task_started') == '1'
                              for name in ('first', 'second')))
        self.assertEqual(self.windows(), ['first', 'second'])
        self.assertEqual(len(self.tmux('list-panes', '-a', '-F', '#{pane_id}').splitlines()), 2)

    def test_code_pool_launcher_creates_an_isolated_task(self):
        self.cli('projects', 'rebuild', session=None)
        text = (self.root / 'config/tmuxinator/app.yml').read_text()
        line = next(line for line in text.splitlines() if line.startswith('        - '))
        command = line.strip()[2:][1:-1].replace("''", "'")
        self.tmux('new-session', '-d', '-s', 'app', '-n', 'app', '-c', str(self.code / 'app'), command)
        destination = self.workspaces / 'app/tasks/app/app'
        self.wait(lambda: (destination / '.git').exists())
        self.wait(lambda: self.tmux('show-option', '-wqv', '-t', 'app:app', '@workbench_task_started') == '1')
        self.assertNotEqual(destination, self.code / 'app')
        self.assertEqual(self.git(destination, 'branch', '--show-current'), 'task/app/app')
        self.assertEqual(self.tmux('list-windows', '-t', 'app', '-F', '#{window_name}'), 'app')

    def test_done_can_clean_closed_directions_without_touching_main_checkouts(self):
        self.new('first', 'app')
        self.cli('task', 'new', 'second', 'app')
        self.tmux('kill-server')
        self.cli('done', 'direction', session=None)
        self.assertFalse((self.workspaces / 'direction').exists())
        self.assertTrue((self.code / 'app/.git').is_dir())
        self.assertEqual(len(self.git(self.code / 'app', 'worktree', 'list', '--porcelain').split('worktree ')) - 1, 1)


    def test_initializer_and_selected_remote_base_are_preserved(self):
        app = self.code / 'app'
        self.git(app, 'switch', '-qc', 'release/word')
        (app / 'base-file').write_text('release base')
        self.git(app, 'add', 'base-file')
        self.git(app, 'commit', '-qm', 'release')
        self.git(app, 'push', '-q', 'origin', 'release/word')
        expected = self.git(app, 'rev-parse', 'HEAD')
        self.git(app, 'switch', '-q', 'main')
        initializer = app / '.workbench/worktree-init'
        initializer.parent.mkdir()
        initializer.write_text('#!/bin/sh\nprintf "%s|%s\\n" "$WORKBENCH_TASK" "$WORKBENCH_TASK_ROOT" > .initialized\n')
        initializer.chmod(0o755)
        self.new('first')
        self.cli('add', '--base', 'origin/release/word', 'app:custom', task='first')
        checkout = self.task_root('first') / 'app'
        self.assertEqual(self.git(checkout, 'rev-parse', 'HEAD'), expected)
        self.assertEqual((checkout / '.initialized').read_text().strip(), f'first|{self.task_root("first")}')
        self.assertEqual(self.windows(), ['first'])
        initializer.write_text('#!/bin/sh\nexit 23\n')
        result = self.cli('add', 'app:custom', task='first', check=False)
        self.assertEqual(result.returncode, 23)
        self.assertTrue((checkout / '.git').exists())
        initializer.unlink()
        initializer.symlink_to(app / 'file')
        self.assertNotEqual(self.cli('add', 'app:custom', task='first', check=False).returncode, 0)

    def test_legacy_task_and_new_tasks_keep_separate_ownership(self):
        workspace = self.workspaces / 'direction'
        workspace.mkdir()
        self.git(self.code / 'app', 'worktree', 'add', '-qb', 'direction', str(workspace / 'app'))
        pane = self.tmux('new-session', '-d', '-s', 'direction', '-n', 'agent', '-c', str(workspace), '-P', '-F', '#{pane_id}', 'sleep', '300')
        self.tmux('set-option', '-t', 'direction', '@workbench_task', '1')
        self.cli('add', 'api', pane=pane)
        self.assertTrue((workspace / 'api/.git').exists())
        self.cli('task', 'new', 'second', 'app')
        self.assertTrue((self.task_root('second') / 'app/.git').exists())
        self.assertEqual(self.pane('agent'), pane)
        self.assertNotEqual(self.cli('add', 'api', check=False).returncode, 0)
        self.cli('task', 'done', 'direction')
        self.assertFalse((workspace / 'app').exists())
        self.assertTrue((self.task_root('second') / 'app/.git').exists())
        self.assertEqual(self.windows(), ['second'])

    def test_instant_service_exit_closes_the_reused_slot(self):
        self.new('first', 'app')
        pane = self.pane('first')
        service = self.cli('run', '--name', 'dev', str(self.task_root('first') / 'app'), '--', 'sleep', '300', pane=pane).stdout.strip()
        replaced = self.cli('run', '--name', 'dev', str(self.task_root('first') / 'app'), '--', 'true', pane=pane).stdout.strip()
        self.assertEqual(service, replaced)
        self.wait(lambda: service not in self.tmux('list-panes', '-a', '-F', '#{pane_id}').splitlines())
        self.assertEqual(self.windows(), ['first'])

    def test_retired_promote_never_moves_a_live_pane(self):
        self.new('first', 'app')
        pane = self.pane('first')
        result = self.cli('promote', 'another-direction', pane=pane, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('task new', result.stderr)
        self.assertEqual(self.windows(), ['first'])
        self.assertEqual(self.pane('first'), pane)
        self.assertFalse((self.workspaces / 'another-direction').exists())

    def test_task_names_are_literal_and_survive_project_generation(self):
        self.new('first')
        name = "中文任务 O'Reilly $(touch injected)"
        self.cli('task', 'new', name)
        self.assertTrue(self.task_root(name).is_dir())
        self.assertFalse((self.root / 'injected').exists())
        text = (self.root / 'config/tmuxinator/direction.yml').read_text()
        command = [line.strip()[2:][1:-1].replace("''", "'")
                   for line in text.splitlines() if line.startswith('        - ')][-1]
        args = shlex.split(command)
        self.assertEqual(args[-1], name)


    def test_task_prompt_from_background_binding_creates_only_one_task(self):
        self.new('first')
        original = self.pane('first')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
        client = subprocess.Popen([str(self.bin / 'tmux'), 'attach-session', '-t', 'direction'],
                                  env=dict(self.env, TERM='xterm-256color'),
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        client_name = ''
        try:
            self.wait(lambda: bool(self.tmux('list-clients', '-F', '#{client_name}')))
            client_name = self.tmux('list-clients', '-F', '#{client_name}')
            command = shlex.join(['env', 'TMUX_PANE=' + original,
                                  'WORKBENCH_TARGET_CLIENT=' + client_name,
                                  str(CLI), 'task', 'new', '--prompt'])
            self.tmux('run-shell', '-b', command)
            output = bytearray()
            deadline = time.monotonic() + 8
            while b'Task name:' not in output:
                if time.monotonic() >= deadline:
                    self.fail('task name popup did not open')
                if select.select([master], [], [], .1)[0]:
                    output.extend(os.read(master, 65536))
            os.write(master, b'parallel task\r')
            self.wait(lambda: 'parallel task' in self.windows())
            self.assertEqual(self.windows(), ['first', 'parallel task'])
            self.assertEqual(self.pane('first'), original)
        finally:
            self.tmux('detach-client', '-t', client_name, check=False)
            client.terminate()
            try:
                client.wait(timeout=2)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)
            os.close(master)
            os.close(slave)


if __name__ == '__main__':
    unittest.main()
