"""Exercise Workbench's session-less relaunch controls with an isolated ledger."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

CLI = Path(__file__).resolve().parents[1] / 'bin/tmux-agent-workbench-cli'


class AssistantAdmin(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='wb-relaunch-admin-')
        self.root = Path(self.temp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        tmux = self.bin / 'tmux'
        tmux.write_text('#!/bin/sh\nexit 1\n')
        tmux.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.root), TMUX_RESURRECT_DIR=str(self.root / 'save'),
                        PATH=str(self.bin) + os.pathsep + os.environ['PATH'])
        self.env.pop('TMUX', None)
        self.env.pop('TMUX_PANE', None)
        self.env.pop('TMUX_AGENT_WORKBENCH_TMUX_SOCKET', None)
        (self.root / 'save').mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def call(self, *args):
        return subprocess.run([str(CLI), 'resurrect', *args], env=self.env, text=True,
                              capture_output=True, timeout=10)

    def test_candidates_seed_and_exact_authorization(self):
        directory = self.root / 'save'
        (directory / 'assistant-relaunch-candidates.json').write_text(json.dumps([
            {'tool': 'claude', 'cmd': 'claude agents', 'seen': 2, 'longest_seconds': 80},
            {'tool': 'claude', 'cmd': 'claude agents --yolo', 'seen': 1, 'longest_seconds': 1},
        ]))
        result = self.call('relaunch', 'candidates')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('claude agents', result.stdout)
        self.assertEqual(self.call('relaunch', 'seed').returncode, 0)
        voucher = directory / 'assistant-relaunch-allow.txt'
        self.assertEqual(voucher.stat().st_mode & 0o777, 0o600)
        self.assertNotIn('claude agents\n', voucher.read_text())
        self.assertNotEqual(self.call('relaunch', 'add', 'claude gateway').returncode, 0)
        self.assertNotEqual(self.call('relaunch', 'add', 'claude agents --yolo').returncode, 0)
        self.assertEqual(self.call('relaunch', 'add', 'claude agents').returncode, 0)
        self.assertEqual(self.call('relaunch', 'add', 'claude agents').returncode, 0)
        self.assertEqual(voucher.read_text().splitlines().count('claude agents'), 1)
        self.assertEqual(self.call('relaunch', 'add', 'claude agents --yolo', '--force').returncode, 0)

    def test_status_reads_historical_sidecar(self):
        (self.root / 'save/assistant-sessions.json').write_text(json.dumps({
            'timestamp': 'earlier', 'sessions': [{'tool': 'codex', 'pane': 's:0.0', 'session_id': 'id'}],
        }))
        result = self.call('status')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Last save: earlier (1 session(s))', result.stdout)
        self.assertIn('codex in s:0.0: id', result.stdout)

    def test_legacy_paths_resolve_to_owned_module(self):
        repo = CLI.parent.parent
        old = repo / 'vendor/tmux-assistant-resurrect'
        self.assertTrue(old.is_symlink())
        self.assertEqual(old.resolve(), (repo / 'lib/assistant-sessions').resolve())
        self.assertTrue((old / 'hooks/claude-session-track.sh').is_file())
        self.assertTrue((old / 'scripts/restore-assistant-sessions.sh').is_file())
        for name in ('workbench-resurrect-save-all', 'workbench-resurrect-restore-all'):
            self.assertNotIn('vendor/tmux-assistant-resurrect', (repo / 'bin' / name).read_text())

    def test_clean_only_reaps_stale_assistant_state(self):
        state = self.root / 'assistant-state'
        state.mkdir()
        self.env['TMUX_ASSISTANT_RESURRECT_DIR'] = str(state)
        stale = state / 'claude-99999999.json'
        live = state / ('opencode-' + str(os.getpid()) + '.json')
        unrelated = state / 'other.json'
        for path in (stale, live, unrelated):
            path.write_text('{}')
        result = self.call('clean')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(stale.exists())
        self.assertTrue(live.exists())
        self.assertTrue(unrelated.exists())

    def test_claude_hook_tracks_and_cleans_configured_environment(self):
        (self.bin / 'tmux').write_text('#!/bin/sh\nprintf "%s\\n" TEST_CAPTURE\n')
        ps = self.bin / 'ps'
        ps.write_text('#!/bin/sh\nprintf "%s\\n" claude\n')
        ps.chmod(0o755)
        self.env['TMUX_ASSISTANT_RESURRECT_DIR'] = str(self.root / 'assistant-state')
        self.env['TEST_CAPTURE'] = 'fixture-value'
        result = subprocess.run([str(CLI), 'hook', 'track', 'claude', 'start'], env=self.env,
                                input='{"session_id":"session-one"}', text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        state = self.root / 'assistant-state' / ('claude-' + str(os.getpid()) + '.json')
        self.assertEqual(json.loads(state.read_text())['env']['TEST_CAPTURE'], 'fixture-value')
        result = subprocess.run([str(CLI), 'hook', 'track', 'claude', 'end'], env=self.env,
                                input='{}', text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(state.exists())
