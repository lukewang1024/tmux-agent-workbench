import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which('tmux'), 'tmux is required')
class MetricsPluginTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        socket = str(Path(self.tmp.name) / 'tmux.sock')
        self.cmd = ['tmux', '-S', socket]
        subprocess.run(self.cmd + ['-f', '/dev/null', 'new-session', '-d',
                                  '-s', 'metrics-test', '/bin/sh'], check=True)
        self.addCleanup(lambda: subprocess.run(self.cmd + ['kill-server'],
                                               capture_output=True))
        self.env = dict(os.environ, TMUX=f'{socket},0,0', CURRENT_DIR=str(ROOT))

    def option(self, name):
        return subprocess.check_output(self.cmd + ['show-option', '-gqv', name],
                                       text=True).strip()

    def set_option(self, name, value):
        subprocess.run(self.cmd + ['set-option', '-g', name, value], check=True)

    def load(self):
        subprocess.run(['sh', str(ROOT / 'lib/host-metrics.sh')],
                       env=self.env, check=True)

    def test_default_registration_and_reload(self):
        self.set_option('@adaptive_cpu', '#(sh /old/dotfiles/tmux-host-metrics cpu)')
        self.set_option('status-right', 'custom-theme')
        self.load()
        self.assertEqual(self.option('@workbench-host-metrics'), 'on')
        self.assertEqual(self.option('@workbench-host-metrics-mode'), 'compact')
        self.assertEqual(self.option('@adaptive_cpu_min_width'), '80')
        widget = self.option('@adaptive_cpu')
        self.assertIn(str(ROOT / 'bin/workbench-host-metrics-status'), widget)
        self.assertNotIn('dotfiles', widget)
        self.load()
        self.assertEqual(self.option('@adaptive_cpu'), widget)
        self.assertEqual(self.option('status-right'), 'custom-theme')

    def test_overrides_disable_and_reenable(self):
        self.set_option('@workbench-host-metrics-min-width', '90')
        self.set_option('@workbench-host-metrics-full-min-width', '160')
        self.set_option('@workbench-host-metrics-mode', 'standard')
        self.load()
        self.assertEqual(self.option('@adaptive_cpu_min_width'), '90')
        self.assertEqual(self.option('@workbench-host-metrics-mode'), 'standard')
        self.assertIn(str(ROOT / 'bin/workbench-host-metrics-status'), self.option('@adaptive_cpu'))
        self.set_option('@workbench-host-metrics', 'off')
        self.load()
        self.assertEqual(self.option('@adaptive_cpu'), '')
        self.set_option('@workbench-host-metrics', 'on')
        self.load()
        self.assertIn(str(ROOT / 'bin/workbench-host-metrics-status'), self.option('@adaptive_cpu'))


if __name__ == '__main__':
    unittest.main()
