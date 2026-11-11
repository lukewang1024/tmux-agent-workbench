import os
import fcntl
from pathlib import Path
import pty
import select
import shlex
import signal
import shutil
import struct
import subprocess
import tempfile
import termios
import time
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

    def expand(self, option):
        return subprocess.check_output(self.cmd + ['display-message', '-p',
                                                  '#{E:' + option + '}'],
                                       text=True).rstrip('\n')

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

    def test_cached_status_job_does_not_sample_when_disabled(self):
        self.set_option('@workbench-host-metrics', 'off')
        cache = Path(self.tmp.name) / 'unused-cache'
        env = dict(self.env, XDG_CACHE_HOME=str(cache))
        output = subprocess.check_output(['sh', str(ROOT / 'bin/workbench-host-metrics-status')], env=env)
        self.assertEqual(output, b'')
        self.assertFalse(cache.exists())

    def test_status_preserves_mode_when_enable_option_is_unset(self):
        self.set_option('@workbench-host-metrics-mode', 'standard')
        cache = Path(self.tmp.name) / 'cache'
        snapshot = cache / 'tmux-agent-workbench/host-metrics/snapshot'
        snapshot.parent.mkdir(parents=True)
        snapshot.write_text('v1 1000 3 25% 35% 40% -- -- 1000 100 50 0 - - -\n')
        shim = Path(self.tmp.name) / 'bin'
        shim.mkdir()
        (shim / 'date').write_text('#!/bin/sh\necho 1000\n')
        (shim / 'date').chmod(0o755)
        env = dict(self.env, XDG_CACHE_HOME=str(cache), PATH=str(shim) + ':' + os.environ['PATH'])
        output = subprocess.check_output(['sh', str(ROOT / 'bin/workbench-host-metrics-status')], env=env, text=True)
        self.assertEqual(output, '󰻠 25% 󰍛 35% 󰋊 40%')

    def test_shared_metrics_update_without_changing_the_menu_mode(self):
        self.load()
        self.set_option('@workbench_status_ready', '1')
        self.set_option('@workbench_status_updated_at', str(int(time.time())))
        self.set_option('@workbench_cached_cpu', 'cpu')
        self.set_option('@workbench_cached_metrics_compact', 'compact')
        self.set_option('@workbench_cached_metrics_standard', 'standard')
        self.set_option('@workbench_cached_metrics_full', 'full')
        self.assertEqual(self.expand('@adaptive_cpu_compact'), 'cpu')
        for mode in ('compact', 'standard', 'full'):
            self.set_option('@workbench-host-metrics-mode', mode)
            self.assertEqual(self.expand('@adaptive_cpu'), mode)
        # The old job remains the fallback until a compatible core publishes.
        self.assertIn('workbench-host-metrics-status', self.option('@adaptive_cpu'))
        self.set_option('@workbench_status_updated_at', str(int(time.time()) - 30))
        self.assertEqual(self.expand('@adaptive_cpu'), '')
        self.set_option('@workbench_status_updated_at', str(int(time.time())))
        self.assertEqual(self.expand('@adaptive_cpu'), 'full')

    def test_cached_usage_changes_with_the_selected_source(self):
        subprocess.run(['sh', str(ROOT / 'lib/status-usage.sh')],
                       env=self.env, check=True)
        self.set_option('@workbench_status_ready', '1')
        self.set_option('@workbench_status_updated_at', str(int(time.time())))
        self.set_option('@adaptive_compact_min_width', '70')
        self.set_option('@workbench_cached_usage_codex', 'codex badge')
        self.set_option('@workbench_cached_usage_claude', 'claude badge')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
        env = dict(self.env, TERM='xterm-256color')
        env.pop('TMUX', None)
        client = subprocess.Popen(self.cmd + ['attach-session', '-t', 'metrics-test'],
                                  stdin=slave, stdout=slave, stderr=slave, env=env)
        try:
            deadline = time.monotonic() + 10
            while True:
                clients = subprocess.check_output(self.cmd + ['list-clients', '-F', '#{client_name}'],
                                                  text=True).strip()
                if clients:
                    break
                self.assertLess(time.monotonic(), deadline, 'tmux client did not attach')
                time.sleep(.1)
            def badge():
                return subprocess.check_output(self.cmd + ['display-message', '-p', '-c', clients,
                                                             '#{E:@adaptive_context_suffix}'],
                                               text=True).rstrip('\n')
            self.set_option('@workbench-usage-source', 'codex')
            self.assertEqual(badge(), 'codex badge')
            self.set_option('@adaptive_compact_min_width', '200')
            self.assertEqual(badge(), '')
            subprocess.run(self.cmd + ['set-option', '-gu', '@adaptive_compact_min_width'], check=True)
            self.assertEqual(badge(), 'codex badge')
            self.set_option('@workbench-usage-source', 'claude')
            self.assertEqual(badge(), 'claude badge')
            self.set_option('@workbench-usage-source', 'opencode')
            self.assertEqual(badge(), '')
            self.set_option('@workbench-usage-source', 'codex')
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 50, 0, 0))
            os.kill(client.pid, signal.SIGWINCH)
            deadline = time.monotonic() + 10
            while subprocess.check_output(self.cmd + ['display-message', '-p', '-c', clients,
                                                       '#{client_width}'], text=True).strip() != '50':
                self.assertLess(time.monotonic(), deadline, 'tmux client width did not update')
                time.sleep(.1)
            self.set_option('@adaptive_compact_min_width', '0')
            self.assertEqual(badge(), 'codex badge')
            self.set_option('@adaptive_compact_min_width', 'invalid')
            self.assertEqual(badge(), '')
        finally:
            client.terminate()
            client.wait(timeout=5)
            os.close(master)
            os.close(slave)

    def test_stale_shared_status_falls_back_then_returns_to_cache(self):
        plugin = Path(self.tmp.name) / "plugin with space's"
        (plugin / 'bin').mkdir(parents=True)
        trace = Path(self.tmp.name) / 'fallback-jobs'
        script = plugin / 'bin/workbench-host-metrics-status'
        script.write_text('#!/bin/sh\nprintf "fallback\\n" >> ' + shlex.quote(str(trace)) + '\nprintf "fallback"\n')
        script.chmod(0o700)
        usage = plugin / 'bin/workbench-agent-usage'
        usage.write_text('#!/bin/sh\nprintf "usage\\n" >> ' + shlex.quote(str(trace)) + '\nprintf "badge"\n')
        usage.chmod(0o700)
        subprocess.run(['sh', str(ROOT / 'lib/host-metrics.sh')],
                       env=dict(self.env, CURRENT_DIR=str(plugin)), check=True)
        subprocess.run(['sh', str(ROOT / 'lib/status-usage.sh')],
                       env=dict(self.env, CURRENT_DIR=str(plugin)), check=True)
        self.set_option('@workbench_status_ready', '1')
        self.set_option('@workbench_status_updated_at', str(int(time.time()) - 30))
        self.set_option('@workbench_cached_metrics_compact', 'fresh')
        self.set_option('status-interval', '1')
        self.set_option('status-right', '#{E:@adaptive_cpu} #{E:@adaptive_context_suffix}')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
        env = dict(self.env, TERM='xterm-256color')
        env.pop('TMUX', None)
        client = subprocess.Popen(self.cmd + ['attach-session', '-t', 'metrics-test'],
                                  stdin=slave, stdout=slave, stderr=slave, env=env)
        try:
            deadline = time.monotonic() + 15
            while not trace.exists() or not {'fallback', 'usage'} <= set(trace.read_text().splitlines()):
                self.assertLess(time.monotonic(), deadline, 'stale status did not run fallback')
                if select.select([master], [], [], .1)[0]:
                    os.read(master, 65536)
            self.set_option('@workbench_status_updated_at', str(int(time.time())))
            self.assertEqual(self.expand('@adaptive_cpu'), 'fresh')
        finally:
            client.terminate()
            client.wait(timeout=5)
            os.close(master)
            os.close(slave)


if __name__ == '__main__':
    unittest.main()
