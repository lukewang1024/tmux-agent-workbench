import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'bin/workbench-host-metrics'


class MetricsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.proc = self.root / 'proc'
        (self.proc / 'net').mkdir(parents=True)
        self.env = dict(os.environ, XDG_CACHE_HOME=str(self.root / 'cache'),
                        TMUX_METRICS_PROC_ROOT=str(self.proc),
                        PATH=str(self.bin) + ':' + os.environ['PATH'])
        self.mock('uname', 'echo Linux')
        self.mock('date', 'echo 1000')
        self.mock('df', "printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n/dev/root 100 40 60 40%% /\\n'")
        self.mock('ip', 'exit 1')
        self.mock('core', 'exit 1')
        self.env['TMUX_AGENT_WORKBENCH_BIN'] = str(self.bin / 'core')

    def mock(self, name, body):
        p = self.bin / name
        p.write_text('#!/bin/sh\n' + body + '\n')
        p.chmod(0o755)

    def sample(self, total=100, idle=50, rx=1000, tx=2000, iface='eth0'):
        (self.proc / 'stat').write_text(f'cpu {total-idle} 0 0 {idle} 0 0 0 0 0 0\n')
        (self.proc / 'meminfo').write_text('MemTotal: 1000 kB\nMemAvailable: 600 kB\n')
        (self.proc / 'net/route').write_text(f'{iface} 00000000 00000000 0003 0 0 100 00000000\n')
        (self.proc / 'net/dev').write_text(f'  {iface}: {rx} 0 0 0 0 0 0 0 {tx} 0 0 0 0 0 0 0\n')

    def run_metrics(self, mode='full'):
        return subprocess.check_output(['/bin/sh', str(SCRIPT), mode], env=self.env, text=True)

    def test_linux_delta_and_cache(self):
        self.sample()
        self.assertEqual(self.run_metrics(), '󰻠 -- 󰍛 40% 󰋊 40% 󰕒 -- 󰇚 --')
        self.assertEqual(self.run_metrics('compact'), '󰻠 -- 󰍛 40%')
        self.assertEqual(self.run_metrics('standard'), '󰻠 -- 󰍛 40% 󰋊 40%')
        self.mock('date', 'echo 1002')
        self.sample(total=200, idle=75, rx=5096, tx=4048)
        self.assertEqual(self.run_metrics(), '󰻠 75% 󰍛 40% 󰋊 40% 󰕒 0.00MB/s 󰇚 0.00MB/s')
        self.assertEqual(self.run_metrics('compact'), '󰻠 75% 󰍛 40%')
        self.assertEqual(self.run_metrics('standard'), '󰻠 75% 󰍛 40% 󰋊 40%')
        self.assertEqual(self.run_metrics('cpu'), '󰻠 75%')

    def test_android_restricted_proc(self):
        self.assertEqual(self.run_metrics(), '󰻠 -- 󰍛 -- 󰋊 40% 󰕒 -- 󰇚 --')

    def test_interface_change_and_counter_reset(self):
        self.sample()
        self.run_metrics()
        self.mock('date', 'echo 1002')
        self.sample(total=200, idle=75, iface='wlan0')
        self.assertTrue(self.run_metrics().endswith('󰕒 -- 󰇚 --'))
        self.mock('date', 'echo 1004')
        self.sample(total=10, idle=5, rx=0, tx=0, iface='wlan0')
        self.assertEqual(self.run_metrics(), '󰻠 -- 󰍛 40% 󰋊 40% 󰕒 -- 󰇚 --')

    def test_values_are_compact_across_units_and_unavailable(self):
        self.sample()
        self.run_metrics()
        for index, rate in enumerate([0, 9, 999, 1023, 1024, 1024**2,
                                      1024**3, 1024**4, 1024**6], 1):
            with self.subTest(rate=rate):
                self.mock('date', f'echo {1000 + 2*index}')
                # Seed exact previous counters to test every unit independently.
                before = 998 + 2*index
                (self.root / 'cache/tmux-agent-workbench/host-metrics/snapshot').write_text(
                    f'v1 {before} 3 -- 40% 40% -- -- {before} 100 50 {before} eth0 0 0\n')
                self.sample(total=200, idle=50 if index % 2 else 149,
                            rx=2*rate, tx=2*rate)
                rendered = self.run_metrics()
                self.assertNotIn('  ', rendered)
                cpu_rendered = self.run_metrics('cpu')
                self.assertNotIn('  ', cpu_rendered)
                self.assertTrue(cpu_rendered.startswith('󰻠 '))

    def test_macos_uses_live_deltas_despite_frozen_netstat(self):
        self.mock('uname', 'echo Darwin')
        self.mock('top', "echo 'CPU usage: 10.00% user, 15.00% sys, 75.00% idle'")
        self.mock('sysctl', 'echo 4096000')
        self.mock('vm_stat', "printf 'Mach Virtual Memory Statistics: (page size of 4096 bytes)\\nPages active: 100.\\nPages wired down: 200.\\nPages occupied by compressor: 50.\\n'")
        self.mock('nettop', "printf ',bytes_in,bytes_out,\\napp.1,123456789,987654321,\\n,bytes_in,bytes_out,\\napp.1,2000000,1000000,\\n'")
        self.mock('netstat', "echo 'en0 1500 <Link#1> aa:bb 10 0 1000 20 0 2000 0'")
        self.assertTrue(self.run_metrics().startswith('󰻠 25% 󰍛 35%'))
        self.mock('date', 'echo 1002')
        self.mock('netstat', "printf 'en0 1500 <Link#1> aa:bb 10 0 5096 20 0 4048 0\\nen0 1500 192.168.1 192.168.1.2 10 - 5096 20 - 4048 -\\n'")
        self.assertTrue(self.run_metrics().endswith('󰕒 1.00MB/s 󰇚 2.00MB/s'))

    def test_macos_cpu_mode_uses_native_ticks_and_skips_other_samplers(self):
        self.mock('uname', 'echo Darwin')
        self.mock('core', "printf '100 50\\n'")
        unexpected = self.root / 'unexpected'
        for name in ['top', 'nettop', 'df', 'vm_stat', 'sysctl']:
            self.mock(name, f"printf '%s\\n' {name} >> '{unexpected}'; exit 1")
        self.assertEqual(self.run_metrics('cpu'), '󰻠 --')
        self.mock('date', 'echo 1002')
        self.mock('core', "printf '200 75\\n'")
        self.assertEqual(self.run_metrics('cpu'), '󰻠 75%')
        self.assertFalse(unexpected.exists())

    def test_compact_skips_disk_network_and_fresh_cache_does_not_write(self):
        self.sample()
        unexpected = self.root / 'unexpected'
        for name in ['df', 'ip', 'nettop']:
            self.mock(name, f"printf '%s\\n' {name} >> '{unexpected}'; exit 1")
        self.assertEqual(self.run_metrics('compact'), '󰻠 -- 󰍛 40%')
        cache = self.root / 'cache/tmux-agent-workbench/host-metrics'
        snapshot = cache / 'snapshot'
        before = snapshot.stat()
        self.mock('mkdir', f"printf mkdir >> '{unexpected}'; exit 1")
        self.assertEqual(self.run_metrics('compact'), '󰻠 -- 󰍛 40%')
        self.assertEqual(self.run_metrics('cpu'), '󰻠 --')
        after = snapshot.stat()
        self.assertEqual((before.st_ino, before.st_mtime_ns), (after.st_ino, after.st_mtime_ns))
        self.assertEqual([p.name for p in cache.iterdir()], ['snapshot'])
        self.assertFalse(unexpected.exists())

    def test_mode_upgrade_reuses_cpu_and_preserves_network_baseline(self):
        self.sample(rx=0, tx=0)
        self.run_metrics('cpu')
        self.assertEqual(self.run_metrics('compact'), '󰻠 -- 󰍛 40%')
        self.assertTrue(self.run_metrics('full').endswith('󰕒 -- 󰇚 --'))
        self.mock('date', 'echo 1002')
        self.sample(total=200, idle=75, rx=2000000, tx=4000000)
        self.assertEqual(self.run_metrics('compact'), '󰻠 75% 󰍛 40%')
        self.mock('date', 'echo 1004')
        self.sample(total=300, idle=100, rx=4000000, tx=8000000)
        self.assertEqual(self.run_metrics('full'), '󰻠 75% 󰍛 40% 󰋊 40% 󰕒 2.00MB/s 󰇚 1.00MB/s')


if __name__ == '__main__':
    unittest.main()
