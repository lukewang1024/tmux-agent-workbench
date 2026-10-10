"""Shutdown acknowledgement must not race workspace cleanup."""
import os
from pathlib import Path
import tempfile
import unittest

from daemon_fixture import running_daemon


class DaemonFixtureTest(unittest.TestCase):
    def check_shutdown(self, body_fails):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            core = root / 'core'
            core.write_text('''#!/bin/sh
set -eu
case $2 in
  run)
    touch "$FIXTURE_ROOT/ready"
    while [ ! -f "$FIXTURE_ROOT/stop" ]; do sleep 1; done
    # Model a state writer that outlives the stop command's reply.
    sleep 1
    touch "$FIXTURE_ROOT/final-write"
    ;;
  status) test -f "$FIXTURE_ROOT/ready" ;;
  stop) touch "$FIXTURE_ROOT/stop" ;;
  *) exit 2 ;;
esac
''')
            core.chmod(0o755)
            env = dict(os.environ, FIXTURE_ROOT=str(root))
            daemon = None
            try:
                with running_daemon(str(core), env) as daemon:
                    if body_fails:
                        raise RuntimeError('test body failed')
            except RuntimeError as error:
                self.assertTrue(body_fails)
                self.assertEqual(str(error), 'test body failed')
            else:
                self.assertFalse(body_fails, 'fixture swallowed the test failure')
            self.assertIsNotNone(daemon)
            self.assertEqual(daemon.poll(), 0)
            self.assertTrue((root / 'final-write').exists())

    def test_waits_for_final_write_before_workspace_cleanup(self):
        self.check_shutdown(False)

    def test_reaps_writer_when_test_body_fails(self):
        self.check_shutdown(True)
