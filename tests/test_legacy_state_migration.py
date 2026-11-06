"""Keep a running legacy tracker writable during session-state migration."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
SAVE = REPO / 'lib/assistant-sessions/scripts/save-assistant-sessions.sh'


class LegacyStateMigration(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='wb-legacy-state-')
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.legacy = self.root / 'legacy'
        self.legacy.mkdir()
        self.destination = self.home / '.local/state/tmux-assistant-resurrect'
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        tmux = self.bin / 'tmux'
        tmux.write_text('#!/bin/sh\nexit 1\n')
        tmux.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.home), TMPDIR=str(self.root),
                        TMUX_RESURRECT_DIR=str(self.root / 'resurrect'),
                        PATH=str(self.bin) + os.pathsep + os.environ['PATH'])
        for key in ('TMUX', 'TMUX_PANE', 'TMUX_ASSISTANT_RESURRECT_DIR', 'XDG_RUNTIME_DIR'):
            self.env.pop(key, None)

    def tearDown(self):
        self.temp.cleanup()

    def sync(self):
        result = subprocess.run(['bash', '-c',
                                 'source "$1"; legacy_path=$2; '
                                 'legacy_assistant_state_dirs() { printf "%s\\n" "$legacy_path"; }; '
                                 'migrate_legacy_state_files',
                                 'workbench-test', str(SAVE), str(self.legacy)],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def live_file(self):
        return self.legacy / ('opencode-' + str(os.getpid()) + '.json')

    def test_live_writer_keeps_directory_and_refreshes_session(self):
        source = self.live_file()
        source.write_text(json.dumps({'session_id': 'first'}))
        source.chmod(0o644)
        self.sync()
        target = self.destination / source.name
        self.assertTrue(source.is_file())
        self.assertEqual(json.loads(target.read_text())['session_id'], 'first')
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)
        newer = target.stat().st_mtime_ns + 2_000_000_000
        source.write_text(json.dumps({'session_id': 'second'}))
        os.utime(source, ns=(newer, newer))
        self.sync()
        self.assertTrue(source.is_file())
        self.assertTrue(self.legacy.is_dir())
        self.assertEqual(json.loads(target.read_text())['session_id'], 'second')

    def test_partial_live_write_does_not_replace_valid_session(self):
        source = self.live_file()
        source.write_text(json.dumps({'session_id': 'valid'}))
        self.sync()
        target = self.destination / source.name
        newer = target.stat().st_mtime_ns + 2_000_000_000
        source.write_text('{"session_id":')
        os.utime(source, ns=(newer, newer))
        self.sync()
        self.assertTrue(source.is_file())
        self.assertEqual(json.loads(target.read_text())['session_id'], 'valid')

    def test_older_live_file_is_preserved_without_overwriting_newer_state(self):
        source = self.live_file()
        source.write_text(json.dumps({'session_id': 'older'}))
        self.destination.mkdir(parents=True)
        target = self.destination / source.name
        target.write_text(json.dumps({'session_id': 'newer'}))
        older = target.stat().st_mtime_ns - 2_000_000_000
        os.utime(source, ns=(older, older))
        self.sync()
        self.assertTrue(source.is_file())
        self.assertEqual(json.loads(target.read_text())['session_id'], 'newer')

    def test_exited_writer_is_migrated_as_before(self):
        source = self.legacy / 'claude-99999999.json'
        source.write_text(json.dumps({'session_id': 'ended'}))
        self.sync()
        self.assertFalse(source.exists())
        self.assertEqual(json.loads((self.destination / source.name).read_text())['session_id'], 'ended')

    def test_failed_live_copy_keeps_original(self):
        source = self.live_file()
        source.write_text(json.dumps({'session_id': 'keep'}))
        copy = self.bin / 'cp'
        copy.write_text('#!/bin/sh\nexit 1\n')
        copy.chmod(0o755)
        self.sync()
        self.assertTrue(source.is_file())
        self.assertFalse((self.destination / source.name).exists())


if __name__ == '__main__':
    unittest.main()
