"""Task isolation and temporary tools against a private, config-free tmux server."""
import os
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

    def test_grouped_tasks_keep_workspaces_and_cleanup_uses_task_identity(self):
        self.new('first', 'app')
        self.cli('task', 'new', 'second', 'app')
        first, second = self.pane('first'), self.pane('second')
        workspace = self.workspaces / 'direction'
        self.tmux('new-session', '-d', '-s', 'display-group', '-n', 'human-shell')
        self.tmux('set-option', '-w', '-t', first, '@workbench_workspace_root', str(workspace))
        self.tmux('move-window', '-s', first, '-t', 'display-group:')
        self.cli('task', 'new', 'first', 'app')
        self.assertEqual(self.tmux('display-message', '-p', '-t', first, '#{session_name}'), 'display-group')
        self.assertEqual(self.windows(), ['second'])
        self.cli('add', 'api', task='first', pane=first)
        service = self.cli('run', str(self.task_root('first') / 'app'), '--', 'sleep', '300',
                           task='first', pane=first).stdout.strip()
        self.assertEqual(self.tmux('display-message', '-p', '-t', service, '#{window_id}'),
                         self.tmux('display-message', '-p', '-t', first, '#{window_id}'))
        self.cli('task', 'done', 'first')
        self.assertFalse(self.task_root('first').exists())
        self.assertEqual(self.tmux('list-windows', '-t', 'display-group', '-F', '#{window_name}'), 'human-shell')
        self.tmux('set-option', '-w', '-t', second, '@workbench_workspace_root', str(workspace))
        self.tmux('move-window', '-s', second, '-t', 'display-group:')
        # The launch session is now gone but its inherited selectors still work.
        self.cli('add', 'api', task='second', pane=second)
        self.cli('done', 'direction', session=None)
        self.assertFalse(workspace.exists())
        self.assertEqual(self.tmux('list-windows', '-t', 'display-group', '-F', '#{window_name}'), 'human-shell')

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

    def test_inspect_file_defaults_to_editor_and_preserves_literal_filename(self):
        self.new('first', 'app')
        caller = self.pane('first')
        file = self.task_root('first') / "app/notes ' $(touch injected).md"
        file.write_text('# Preview\n')
        record = self.root / 'editor-args'
        self.write_script('preview-editor',
                          f'printf "%s\\n" "$PWD" "$@" > {shlex.quote(str(record))}\nexec sleep 300\n')
        tool = self.cli('inspect', str(file), '--pane', pane=caller,
                        extra={'VISUAL': f'{self.bin / "preview-editor"} --marker',
                               'EDITOR': 'false'}).stdout.strip()
        self.wait(record.exists)
        self.assertEqual(record.read_text().splitlines(),
                         [str(file.parent), '--marker', '--', str(file)])
        self.assertFalse((file.parent / 'injected').exists())
        self.assertEqual(self.tmux('display-message', '-p', '-t', tool, '#{window_id}'),
                         self.tmux('display-message', '-p', '-t', caller, '#{window_id}'))
        self.assertEqual(self.windows(), ['first'])

    def test_inspect_file_rejects_other_tasks_and_escaping_symlinks(self):
        self.new('first', 'app')
        self.cli('task', 'new', 'second', 'app')
        caller = self.pane('first')
        foreign = self.task_root('second') / 'app/file'
        symlink = self.task_root('first') / 'app/escape.md'
        symlink.symlink_to(foreign)
        for path in (foreign, symlink):
            with self.subTest(path=path):
                result = self.cli('inspect', str(path), '--pane', pane=caller, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('not a checkout owned by task first', result.stderr)
        result = self.cli('inspect', str(self.task_root('first') / 'app/file'),
                          '--git', '--pane', pane=caller, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}'), caller)

    def test_inspect_file_works_in_an_ordinary_session(self):
        caller = self.tmux('new-session', '-d', '-s', 'ordinary', '-P', '-F',
                           '#{pane_id}', 'sleep', '300')
        file = self.root / 'outside-repo.md'
        file.write_text('# Outside a workspace\n')
        record = self.root / 'editor-args'
        self.write_script('preview-editor',
                          f'printf "%s\\n" "$@" > {shlex.quote(str(record))}\nexec sleep 300\n')
        self.cli('inspect', str(file), '--editor', '--pane', session=None, pane=caller,
                 extra={'VISUAL': '', 'EDITOR': str(self.bin / 'preview-editor')})
        self.wait(record.exists)
        self.assertEqual(record.read_text().splitlines(), ['--', str(file)])

    def test_inspect_command_receives_literal_arguments_and_interactive_terminal(self):
        self.new('first', 'app')
        caller = self.pane('first')
        directory = self.task_root('first') / 'app'
        record = self.root / 'command-args'
        answer = self.root / 'command-answer'
        self.write_script('interactive-tool',
                          '[ -t 0 ] && [ -t 1 ]\n'
                          '[ -n "${HTTPS_PROXY:-}" ] && [ -n "${https_proxy:-}" ]\n'
                          f'printf "%s\\n" "$PWD" "$@" > {shlex.quote(str(record))}\n'
                          'printf "Waiting for input: "\n'
                          'IFS= read -r answer\n'
                          f'printf "%s\\n" "$answer" > {shlex.quote(str(answer))}\n')
        args = ['space here', "quote's", '', '$(touch injected)', '; touch injected', '--pane', '预览']
        tool = self.cli('inspect', str(directory), '--pane', '--', str(self.bin / 'interactive-tool'),
                        *args, pane=caller, extra={'HTTPS_PROXY': 'http://test-proxy.invalid:8443',
                                                  'https_proxy': ''}).stdout.strip()
        self.wait(record.exists)
        self.assertEqual(record.read_text().splitlines(), [str(directory), *args])
        self.assertFalse((directory / 'injected').exists())
        self.assertIn(tool, self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}').splitlines())
        self.tmux('send-keys', '-t', tool, '-l', 'user input')
        self.tmux('send-keys', '-t', tool, 'Enter')
        self.wait(answer.exists)
        self.assertEqual(answer.read_text(), 'user input\n')
        self.wait(lambda: tool not in self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}').splitlines())
        self.assertEqual(self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}'), caller)
        self.assertEqual(self.windows(), ['first'])

    def test_inspect_command_exit_closes_the_pane_on_success_or_failure(self):
        self.new('first', 'app')
        caller = self.pane('first')
        self.tmux('set-option', '-w', '-t', caller, 'remain-on-exit', 'on')
        for status in (0, 17):
            with self.subTest(status=status):
                tool = self.cli('inspect', str(self.task_root('first') / 'app'), '--pane', '--',
                                '/bin/sh', '-c', f'exit {status}', pane=caller).stdout.strip()
                self.wait(lambda: tool not in self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}').splitlines())
                self.assertEqual(self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}'), caller)

    def test_inspect_command_rejects_invalid_modes_and_foreign_directories(self):
        self.new('first', 'app')
        caller = self.pane('first')
        directory = self.task_root('first') / 'app'
        for args in (('--',), ('--git', '--', 'true'), ('--editor', '--', 'true'),
                     ('--shell', '--', 'true')):
            with self.subTest(args=args):
                result = self.cli('inspect', str(directory), '--pane', *args, pane=caller, check=False)
                self.assertEqual(result.returncode, 2)
        result = self.cli('inspect', str(directory / 'file'), '--pane', '--', 'true',
                          pane=caller, check=False)
        self.assertEqual(result.returncode, 2)
        result = self.cli('inspect', str(self.code / 'app'), '--pane', '--', 'true',
                          pane=caller, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not a checkout owned by task first', result.stderr)
        self.assertEqual(self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}'), caller)

    def test_inspect_panes_share_right_column_and_preserve_sidebar(self):
        self.new('first', 'app')
        caller = self.pane('first')
        self.tmux('resize-window', '-t', caller, '-x', '200', '-y', '60')
        directory = self.task_root('first') / 'app'

        def geometry(pane):
            return tuple(map(int, self.tmux('display-message', '-p', '-t', pane,
                                           '#{pane_left} #{pane_top} #{pane_width} #{pane_height}').split()))

        for position in ('none', 'left', 'right'):
            with self.subTest(sidebar=position):
                sidebar = None
                if position != 'none':
                    self.tmux('set-option', '-g', '@sidebar_width', '18')
                    self.tmux('set-option', '-g', '@sidebar_position', position)
                    args = ['split-window', '-d', '-h', '-f', '-l', '18', '-t', caller]
                    if position == 'left':
                        args.append('-b')
                    sidebar = self.tmux(*args, '-P', '-F', '#{pane_id}', 'sleep', '300')
                    self.tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
                tools = [self.cli('inspect', str(directory), '--pane', pane=caller,
                                  extra={'WORKBENCH_GIT_TOOL': 'sleep 300'}).stdout.strip()
                         for _ in range(3)]
                main = geometry(caller)
                peers = [geometry(tool) for tool in tools]
                self.assertEqual(main[0], 19 if position == 'left' else 0)
                self.assertEqual(main[1], 0)
                self.assertEqual(main[3], 60)
                self.assertTrue(all(peer[0] == main[0] + main[2] + 1 for peer in peers))
                self.assertEqual(len({peer[2] for peer in peers}), 1)
                self.assertLessEqual(max(peer[3] for peer in peers) - min(peer[3] for peer in peers), 1)
                self.assertEqual(sum(peer[3] for peer in peers) + 2, main[3])
                self.assertEqual(self.tmux('display-message', '-p', '-t', caller, '#{pane_active}'), '1')
                if sidebar:
                    self.assertEqual(geometry(sidebar), (0 if position == 'left' else 182, 0, 18, 60))
                # Exiting a middle tool must rebalance the remaining column
                # while leaving the user's focus in another preview pane.
                self.tmux('select-pane', '-t', tools[0])
                self.tmux('send-keys', '-t', tools[1], 'C-c')
                self.wait(lambda: tools[1] not in self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}').splitlines())
                self.wait(lambda: abs(geometry(tools[0])[3] - geometry(tools[2])[3]) <= 1)
                self.assertEqual(self.tmux('display-message', '-p', '-t', tools[0], '#{pane_active}'), '1')
                self.assertEqual(self.windows(), ['first'])
                for tool in (tools[0], tools[2]):
                    self.tmux('kill-pane', '-t', tool)
                if sidebar:
                    self.tmux('kill-pane', '-t', sidebar)

    @unittest.skipUnless(shutil.which('vim'), 'Vim is required for the editor smoke test')
    def test_quitting_vim_closes_preview_even_with_remain_on_exit(self):
        self.new('first', 'app')
        caller = self.pane('first')
        self.tmux('resize-window', '-t', caller, '-x', '180', '-y', '40')
        self.tmux('set-option', '-w', '-t', caller, 'remain-on-exit', 'on')
        file = self.task_root('first') / 'app/preview.md'
        contents = '# Workbench Markdown preview\n'
        file.write_text(contents)
        tool = self.cli('inspect', str(file), '--pane', pane=caller,
                        extra={'VISUAL': '', 'EDITOR': 'vim -Nu NONE -n -i NONE'}).stdout.strip()
        self.wait(lambda: contents.strip() in self.tmux('capture-pane', '-p', '-t', tool))
        self.tmux('send-keys', '-t', tool, 'Escape', ':q', 'Enter')
        self.wait(lambda: tool not in self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}').splitlines())
        self.assertEqual(self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}'), caller)
        self.assertEqual(file.read_text(), contents)

    def test_tool_exit_keeps_manual_layout_and_existing_tmux_hooks(self):
        self.new('first', 'app')
        caller = self.pane('first')
        self.tmux('resize-window', '-t', caller, '-x', '200', '-y', '60')
        tools = [self.cli('inspect', str(self.task_root('first') / 'app'), '--pane', pane=caller,
                          extra={'WORKBENCH_GIT_TOOL': 'sleep 300'}).stdout.strip()
                 for _ in range(2)]
        self.tmux('set-hook', '-g', 'after-kill-pane[777]',
                  f'set-option -w -t {caller} @test_existing_hook called')
        self.call([REPO / 'bin/workbench-layout', 'even-vertical', caller])
        self.tmux('send-keys', '-t', tools[0], 'C-c')
        self.wait(lambda: tools[0] not in self.tmux('list-panes', '-t', caller, '-F', '#{pane_id}').splitlines())
        self.wait(lambda: self.tmux('show-option', '-wqv', '-t', caller, '@test_existing_hook') == 'called')
        self.assertEqual(self.tmux('show-option', '-wqv', '-t', caller, '@workbench_layout_preset'),
                         'even-vertical')
        for pane in (caller, tools[1]):
            self.assertEqual(self.tmux('display-message', '-p', '-t', pane, '#{pane_left} #{pane_width}'), '0 200')

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
        commands = [line.strip()[2:][1:-1].replace("''", "'")
                    for line in text.splitlines() if line.startswith('        - ')]
        self.assertCountEqual([shlex.split(command)[-1] for command in commands], ['first', name])


    def test_task_prompt_from_background_binding_creates_only_one_task(self):
        self.new('first')
        original = self.pane('first')
        # This suite owns workflow orchestration. Real modal keyboard/touch
        # behavior is covered by status-menu-mouse.py against the built core.
        self.write_script('core', 'case ${1:-} in prompt-text) printf "%s\\n" "parallel task" ;; esac\n')
        command = shlex.join(['env', 'TMUX_PANE=' + original,
                              str(CLI), 'task', 'new', '--prompt'])
        self.tmux('run-shell', '-b', command)
        self.wait(lambda: 'parallel task' in self.windows())
        self.assertEqual(self.windows(), ['first', 'parallel task'])
        self.assertEqual(self.pane('first'), original)


if __name__ == '__main__':
    unittest.main()
