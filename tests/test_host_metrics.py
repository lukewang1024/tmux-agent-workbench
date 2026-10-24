"""Metrics locks must recover after interrupted initialization without stealing a live lock."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'bin/workbench-host-metrics'


class MetricsLockTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='workbench-metrics-')
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.cache = root / 'tmux-agent-workbench/host-metrics'
        self.cache.mkdir(parents=True)
        self.lock = self.cache / 'lock'
        self.lock.mkdir()
        (self.cache / 'cpu').write_text('old 33%')
        shim = root / 'bin'
        shim.mkdir()
        # Deterministic Linux fixtures run on both macOS and Linux, without
        # launching top/nettop or reading the host's counters.
        (shim / 'uname').write_text("#!/bin/sh\nprintf '%s\\n' Linux\n")
        (shim / 'uname').chmod(0o755)
        if sys.platform == 'darwin':
            (shim / 'stat').write_text('#!/bin/sh\nexec /usr/bin/stat -f %m "$3"\n')
            (shim / 'stat').chmod(0o755)

        proc = root / 'proc'
        (proc / 'net').mkdir(parents=True)
        (proc / 'stat').write_text('cpu 10 0 10 80 0 0 0 0\n')
        (proc / 'meminfo').write_text('MemTotal: 1000 kB\nMemAvailable: 500 kB\n')
        (proc / 'net/route').write_text('eth0 00000000 0 0 0 0 1 00000000\n')
        (proc / 'net/dev').write_text('eth0: 100 0 0 0 0 0 0 0 100 0\n')
        self.env = dict(os.environ, XDG_CACHE_HOME=str(root), TMUX_METRICS_PROC_ROOT=str(proc),
                        PATH=str(shim) + ':' + os.environ['PATH'])

    def sample(self):
        return subprocess.check_output(['/bin/sh', str(SCRIPT), 'cpu'], env=self.env, text=True)

    def test_old_ownerless_lock_recovers_and_releases(self):
        old = time.time() - 60
        os.utime(self.lock, (old, old))
        self.assertNotEqual(self.sample(), 'old 33%')
        self.assertTrue((self.cache / 'snapshot').is_file())
        self.assertFalse(self.lock.exists())

    def test_initializing_owner_keeps_lock(self):
        self.assertEqual(self.sample(), 'old 33%')
        self.assertTrue(self.lock.is_dir())
        self.assertFalse((self.cache / 'snapshot').exists())

    def test_live_owner_keeps_lock_even_when_old(self):
        (self.lock / 'pid').write_text(str(os.getpid()))
        old = time.time() - 60
        os.utime(self.lock, (old, old))
        self.assertEqual(self.sample(), 'old 33%')
        self.assertEqual((self.lock / 'pid').read_text(), str(os.getpid()))


if __name__ == '__main__':
    unittest.main()
