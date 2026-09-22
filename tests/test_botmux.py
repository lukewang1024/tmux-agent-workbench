"""Public-interface adapter must not attach a stale or cross-server pane."""
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest
import os
import tempfile
import subprocess
import shutil
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader('botmux_adapter', str(Path(__file__).resolve().parents[1] / 'bin/workbench-botmux'))
spec = importlib.util.spec_from_loader(loader.name, loader)
adapter = importlib.util.module_from_spec(spec)
loader.exec_module(adapter)


class ConnectTests(unittest.TestCase):
    def test_copies_canonical_target_without_sending_input(self):
        with patch.object(adapter, 'output', side_effect=['/tmp/default', '%2\n100\nwork:1.2', '100']), patch.object(adapter.subprocess, 'run') as run:
            adapter.connect('/tmp/default', '%2', '100', '101')
            self.assertEqual(run.call_args.args[0], ['tmux', '-S', '/tmp/default', 'set-buffer', '-w', '--', '/adopt work:1.2'])
            self.assertEqual(run.call_count, 1)

    def test_rejects_another_server_even_with_same_pane_id(self):
        with patch.object(adapter, 'output', return_value='/tmp/default'), patch.object(adapter.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'different socket'):
                adapter.connect('/tmp/other', '%2', '100', '101')
            run.assert_not_called()

    def test_rejects_replaced_pane(self):
        with patch.object(adapter, 'output', side_effect=['/tmp/default', '%2\n200\nwork:1.2']), patch.object(adapter.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'Pane changed'):
                adapter.connect('/tmp/default', '%2', '100', '101')
            run.assert_not_called()

    def test_rejects_agent_no_longer_in_pane(self):
        with patch.object(adapter, 'output', side_effect=['/tmp/default', '%2\n100\nwork:1.2', '1']), patch.object(adapter.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'Agent changed'):
                adapter.connect('/tmp/default', '%2', '100', '101')
            run.assert_not_called()

    def test_rejects_ambiguous_slash_command(self):
        with patch.object(adapter, 'output', side_effect=['/tmp/default', '%2\n100\nwork space:1.2', '100']), patch.object(adapter.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'session name'):
                adapter.connect('/tmp/default', '%2', '100', '101')
            run.assert_not_called()


@unittest.skipUnless(shutil.which('tmux'), 'tmux required')
class RealTmuxTests(unittest.TestCase):
    def test_default_server_copy_preserves_live_pane(self):
        with tempfile.TemporaryDirectory(prefix='wb-botmux-') as folder:
            env = dict(os.environ, TMUX_TMPDIR=folder)
            env.pop('TMUX', None)
            env.pop('TMUX_PANE', None)
            def tmux(*args):
                return subprocess.check_output(['tmux', *args], env=env, text=True).strip()
            try:
                tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'integration', 'sleep 60')
                socket, pane, root = tmux('display-message', '-p', '#{socket_path}\n#{pane_id}\n#{pane_pid}').splitlines()
                with patch.dict(os.environ, env, clear=True):
                    adapter.connect(socket, pane, root, root)
                self.assertEqual(tmux('show-buffer'), '/adopt integration:0.0')
                self.assertEqual(tmux('display-message', '-p', '-t', pane, '#{pane_pid}:#{pane_dead}'), root + ':0')
                self.assertEqual(tmux('capture-pane', '-p', '-t', pane), '')
            finally:
                subprocess.run(['tmux', 'kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
