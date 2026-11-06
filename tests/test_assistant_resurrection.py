"""Exercise all assistant adapters against a private tmux and fake CLIs."""
import base64
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
CORE = REPO / 'target/debug/tmux-agent-workbench'
RESURRECT = os.environ.get('WORKBENCH_TEST_RESURRECT_SCRIPTS')


class AssistantResurrection(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='wb-assistant-roundtrip-')
        self.root = Path(self.temp.name)
        self.socket = str(self.root / 'tmux.sock')
        self.save_dir = self.root / 'save'
        self.save_dir.mkdir()
        self.home = self.root / 'home'
        self.home.mkdir()
        self.shim = self.root / 'bin'
        self.shim.mkdir()
        self.output = self.root / 'launched.jsonl'
        self.real_tmux = shutil.which('tmux')
        self.env = dict(os.environ, HOME=str(self.home),
                        XDG_CONFIG_HOME=str(self.root / 'config'),
                        XDG_DATA_HOME=str(self.root / 'data'),
                        XDG_STATE_HOME=str(self.root / 'state'),
                        XDG_CACHE_HOME=str(self.root / 'cache'),
                        XDG_RUNTIME_DIR=str(self.root / 'runtime'),
                        TMUX_AGENT_WORKBENCH_TMUX_SOCKET=self.socket,
                        TMUX_RESURRECT_DIR=str(self.save_dir),
                        TMUX_ASSISTANT_RESURRECT_DIR=str(self.root / 'assistant-state'),
                        COPILOT_HOME=str(self.root / 'copilot'),
                        WORKBENCH_TEST_ASSISTANT_OUTPUT=str(self.output),
                        PATH=str(self.shim) + os.pathsep + os.environ['PATH'])
        self.env.pop('TMUX', None)
        self.env.pop('TMUX_PANE', None)
        fake = self.shim / 'fake-assistant'
        fake.write_text('''#!/usr/bin/env node
const fs = require('fs');
const path = require('path');
const args = process.argv.slice(2);
if (args.includes('--help')) {
  process.stdout.write('--model <model>\\n--resume <id>\\n--session <id>\\n');
  process.exit(0);
}
fs.appendFileSync(process.env.WORKBENCH_TEST_ASSISTANT_OUTPUT,
  JSON.stringify({tool: path.basename(process.argv[1]), args}) + '\\n');
setInterval(() => {}, 100000);
''')
        fake.chmod(0o755)
        for tool in ('claude', 'copilot', 'opencode', 'codex', 'pi', 'omp', 'grok', 'traex'):
            (self.shim / tool).symlink_to(fake)
        # The restored sessions have no attached client in this test. Pretend
        # one exists for the attach wait while every other tmux call stays real.
        tmux_shim = self.shim / 'tmux'
        tmux_shim.write_text('#!/bin/sh\n'
                             'case "$*" in *"list-clients"*) printf "fixture-client\\n"; exit 0;; esac\n'
                             'exec ' + shlex.quote(self.real_tmux) + ' "$@"\n')
        tmux_shim.chmod(0o755)
        self.env['TMUX_AGENT_WORKBENCH_BIN'] = str(CORE)

    def tearDown(self):
        subprocess.run([self.real_tmux, '-S', self.socket, 'kill-server'], env=self.env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.temp.cleanup()

    def tmux(self, *args):
        return subprocess.check_output([self.real_tmux, '-S', self.socket, *args],
                                       env=self.env, text=True).strip()

    def start(self, name):
        self.tmux('-f', '/dev/null', 'new-session', '-d', '-s', name, '-x', '120', '-y', '40', '/bin/sh')
        self.tmux('set-option', '-g', 'default-shell', '/bin/sh')
        self.tmux('set-option', '-g', 'default-command', '/bin/sh')
        self.tmux('set-option', '-g', '@resurrect-dir', str(self.save_dir))
        self.env['TMUX'] = self.socket + ',' + self.tmux('display-message', '-p', '#{pid}') + ',0'

    def configure_hooks(self):
        for option, script in (
            ('@resurrect-hook-post-save-layout', 'workbench-resurrect-save-hook'),
            ('@resurrect-hook-post-save-all', 'workbench-resurrect-save-all'),
            ('@resurrect-hook-pre-restore-all', 'workbench-resurrect-pre-restore'),
            ('@resurrect-hook-post-restore-all', 'workbench-resurrect-restore-all'),
        ):
            self.tmux('set-option', '-g', option, shlex.quote(str(REPO / 'bin' / script)))

    def wait_launches(self, count):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            lines = self.output.read_text().splitlines() if self.output.exists() else []
            if len(lines) >= count:
                return [json.loads(line) for line in lines]
            time.sleep(.05)
        self.fail(f'expected {count} fake launches, got {lines}')

    @unittest.skipUnless(RESURRECT, 'tmux-resurrect scripts not configured')
    def test_complete_save_and_restore_all_adapters(self):
        sessions = {
            'claude': ('--resume', 'claude-session'),
            'copilot': ('--resume', '11111111-2222-4333-8444-555555555555'),
            'opencode': ('-s', 'ses_opencode'),
            'codex': ('resume', 'codex-session', '--model', 'fixture-model', 'old-prompt'),
            'pi': ('--session', 'pi-session'),
            'omp': ('--resume', 'omp-session'),
            'grok': ('--resume', 'aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee'),
        }
        copilot_state = self.root / 'copilot/session-state' / sessions['copilot'][1]
        copilot_state.mkdir(parents=True)
        (copilot_state / 'session.db').touch()
        self.start('bootstrap')
        self.tmux('new-session', '-d', '-s', 'agents', '-n', 'claude',
                  shlex.join([str(self.shim / 'claude'), *sessions['claude']]))
        for tool, args in list(sessions.items())[1:]:
            self.tmux('new-window', '-d', '-t', 'agents:', '-n', tool,
                      shlex.join([str(self.shim / tool), *args]))
        trae_id = 'traex-session'
        self.tmux('new-window', '-d', '-t', 'agents:', '-n', 'traex',
                  shlex.join([str(self.shim / 'traex'), '--resume', trae_id]))
        trae_window = int(self.tmux('display-message', '-p', '-t', 'agents:traex', '#{window_index}'))
        native_snapshot = self.root / 'native-snapshot.json'
        native_snapshot.write_text(json.dumps({'agents': [{
            'kind': 'traex', 'hook_session_id': trae_id,
            'target': {'session_name': 'agents', 'window_index': trae_window, 'pane_index': 0},
        }]}))
        fake_core = self.shim / 'fake-core'
        fake_core.write_text('#!/bin/sh\n'
                             'if [ "$1" = snapshot ]; then cat ' + shlex.quote(str(native_snapshot)) + '; exit 0; fi\n'
                             'exec ' + shlex.quote(str(CORE)) + ' "$@"\n')
        fake_core.chmod(0o755)
        self.env['TMUX_AGENT_WORKBENCH_BIN'] = str(fake_core)
        self.wait_launches(len(sessions) + 1)
        self.configure_hooks()
        result = subprocess.run([str(Path(RESURRECT) / 'save.sh'), 'quiet'], env=self.env,
                                check=True, capture_output=True, timeout=90)
        save_log = self.save_dir / 'assistant-save.log'
        self.assertTrue((self.save_dir / 'assistant-sessions.json').exists(),
                        (result.stdout.decode(), result.stderr.decode(),
                         save_log.read_text() if save_log.exists() else '', list(self.save_dir.iterdir())))
        saved = json.loads((self.save_dir / 'assistant-sessions.json').read_text())
        self.assertEqual({entry['tool']: entry['session_id'] for entry in saved['sessions']},
                         {tool: args[1] for tool, args in sessions.items()})
        saved_codex = next(entry for entry in saved['sessions'] if entry['tool'] == 'codex')
        self.assertIn('--model', saved_codex['cli_args'])
        self.assertNotIn('old-prompt', saved_codex['cli_args'])
        team_path = self.save_dir / 'workbench-team-sessions.json'
        self.assertTrue(team_path.exists(), result.stderr.decode())
        team = json.loads(team_path.read_text())
        trae = json.loads((self.save_dir / 'workbench-trae-sessions.json').read_text())
        self.assertEqual(trae[0]['session_id'], trae_id)
        for key, path in (('snapshot_sha256', self.save_dir / 'last'),
                          ('assistant_sha256', self.save_dir / 'assistant-sessions.json'),
                          ('trae_sha256', self.save_dir / 'workbench-trae-sessions.json')):
            self.assertEqual(team[key], hashlib.sha256(path.read_bytes()).hexdigest())
        first_snapshot = (self.save_dir / 'last').resolve()
        first_assistants = (self.save_dir / 'assistant-sessions.json').read_bytes()
        archive = self.save_dir / 'workbench-snapshot-sidecars' / (team['snapshot_sha256'] + '.json')
        self.assertTrue(archive.is_file())
        self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
        original_archive = archive.read_bytes()
        damaged = json.loads(original_archive)
        team_data = json.loads(base64.b64decode(damaged['files']['workbench-team-sessions.json']))
        team_data['teams'] = 'not-a-list'
        damaged_team = json.dumps(team_data).encode()
        damaged['files']['workbench-team-sessions.json'] = base64.b64encode(damaged_team).decode()
        for update_checksum in (False, True):
            if update_checksum:
                damaged['team_sha256'] = hashlib.sha256(damaged_team).hexdigest()
            archive.write_text(json.dumps(damaged))
            pre = subprocess.run([str(REPO / 'bin/workbench-resurrect-pre-restore')], env=self.env,
                                 capture_output=True)
            self.assertNotEqual(pre.returncode, 0)
        archive.write_bytes(original_archive)
        # A later complete save must not discard an earlier snapshot's
        # sidecars, even when both saves have exactly the same layout.
        time.sleep(1.1)
        claude_window = int(self.tmux('display-message', '-p', '-t', 'agents:claude', '#{window_index}'))
        claude_pane = self.tmux('display-message', '-p', '-t', 'agents:claude', '#{pane_id}')
        native_snapshot.write_text(json.dumps({'agents': [
            {'kind': 'traex', 'hook_session_id': trae_id,
             'target': {'session_name': 'agents', 'window_index': trae_window, 'pane_index': 0}},
            {'kind': 'claude', 'hook_session_id': 'new-claude-session',
             'target': {'pane_id': claude_pane, 'session_name': 'agents',
                        'window_index': claude_window, 'pane_index': 0}},
        ]}))
        subprocess.run([str(Path(RESURRECT) / 'save.sh'), 'quiet'], env=self.env,
                       check=True, capture_output=True, timeout=90)
        second_snapshot = (self.save_dir / 'last').resolve()
        self.assertNotEqual(second_snapshot, first_snapshot)
        def layout_without_save_id(path):
            return [line for line in path.read_bytes().splitlines()
                    if not line.startswith(b'workbench-assistant-save-id\t')]
        self.assertEqual(layout_without_save_id(first_snapshot), layout_without_save_id(second_snapshot))
        second_assistants = json.loads((self.save_dir / 'assistant-sessions.json').read_text())
        self.assertEqual(next(entry['session_id'] for entry in second_assistants['sessions']
                              if entry['tool'] == 'claude'), 'new-claude-session')
        self.tmux('kill-server')
        self.output.unlink()
        self.env.pop('TMUX', None)
        (self.save_dir / 'last').unlink()
        (self.save_dir / 'last').symlink_to(first_snapshot.name)
        self.start('bootstrap')
        self.configure_hooks()
        restored = subprocess.run([str(Path(RESURRECT) / 'restore.sh'), 'quiet'], env=self.env,
                                  capture_output=True, timeout=90)
        self.assertEqual(restored.returncode, 0, restored.stderr.decode())
        self.assertEqual((self.save_dir / 'assistant-sessions.json').read_bytes(), first_assistants)
        launches = self.wait_launches(len(sessions) + 1)
        self.assertEqual({item['tool'] for item in launches}, set(sessions) | {'traex'})
        for item in launches:
            sid = trae_id if item['tool'] == 'traex' else sessions[item['tool']][1]
            self.assertTrue(any(arg == sid or arg.endswith('=' + sid) for arg in item['args']), item)
            self.assertNotIn('old-prompt', item['args'])
            if item['tool'] == 'codex':
                self.assertIn('fixture-model', item['args'])

    def test_historical_sidecar_without_team_metadata(self):
        self.start('legacy')
        entries = [{'pane': 'legacy:0.0', 'tool': 'claude', 'session_id': 'legacy-session'}]
        (self.save_dir / 'assistant-sessions.json').write_text(json.dumps({'sessions': entries}))
        subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')], env=self.env,
                       check=True, capture_output=True, timeout=30)
        launched = self.wait_launches(1)
        self.assertEqual((launched[0]['tool'], launched[0]['args'][-1]), ('claude', 'legacy-session'))

    def test_new_snapshot_without_team_sidecar_does_not_launch_solo(self):
        self.start('bootstrap')
        (self.save_dir / 'last').write_text('workbench-assistant-sidecars\t1\n')
        (self.save_dir / 'assistant-sessions.json').write_text(json.dumps({'sessions': [
            {'pane': 'agents:0.0', 'tool': 'claude', 'session_id': 'unconfirmed'},
        ]}))
        pre = subprocess.run([str(REPO / 'bin/workbench-resurrect-pre-restore')], env=self.env,
                             capture_output=True)
        self.assertNotEqual(pre.returncode, 0)
        digest = hashlib.sha256((self.save_dir / 'last').read_bytes()).hexdigest()
        archives = self.save_dir / 'workbench-snapshot-sidecars'
        archives.mkdir()
        (archives / (digest + '.json')).write_text('{"version":1,"files":{}}')
        pre = subprocess.run([str(REPO / 'bin/workbench-resurrect-pre-restore')], env=self.env,
                             capture_output=True)
        self.assertNotEqual(pre.returncode, 0)
        self.tmux('new-session', '-d', '-s', 'agents', '-x', '80', '-y', '24', '/bin/sh')
        result = subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')], env=self.env,
                                capture_output=True, timeout=20)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('matching team sidecar', result.stderr.decode())
        self.assertFalse(self.output.exists())

    def test_native_identity_overlay_keeps_options_and_ambiguity(self):
        self.start('agents')
        self.tmux('set-option', '-g', '@resurrect-dir', str(self.root / 'other-save'))
        snapshot = {'agents': [
            {'kind': 'codex', 'hook_session_id': 'new-id',
             'target': {'pane_id': '%1', 'session_name': 'agents', 'window_index': 0, 'pane_index': 0}},
            {'kind': 'claude', 'hook_session_id': 'ignored-ambiguous',
             'target': {'pane_id': '%2', 'session_name': 'agents', 'window_index': 1, 'pane_index': 0}},
            {'kind': 'opencode', 'hook_session_id': 'native-one',
             'target': {'pane_id': '%3', 'session_name': 'agents', 'window_index': 2, 'pane_index': 0}},
            {'kind': 'opencode', 'hook_session_id': 'native-two',
             'target': {'pane_id': '%3', 'session_name': 'agents', 'window_index': 2, 'pane_index': 0}},
        ]}
        fake_core = self.shim / 'fake-core'
        fake_core.write_text('#!/bin/sh\nprintf %s ' + shlex.quote(json.dumps(snapshot)) + '\n')
        fake_core.chmod(0o755)
        self.env['TMUX_AGENT_WORKBENCH_BIN'] = str(fake_core)
        entries = [
            {'pane': 'agents:0.0', 'tool': 'codex', 'session_id': 'old-id',
             'cli_args': '--sandbox read-only', 'env': {'KEEP': 'value'}, 'cwd': '/work'},
            {'pane': 'agents:1.0', 'tool': 'claude', 'session_id': 'one'},
            {'pane': 'agents:1.0', 'tool': 'claude', 'session_id': 'two'},
        ]
        sidecar = self.save_dir / 'assistant-sessions.json'
        sidecar.write_text(json.dumps({'sessions': entries, 'relaunch': []}))
        subprocess.run([str(REPO / 'bin/workbench-resurrect-merge-hooks')], env=self.env,
                       check=True, capture_output=True)
        merged = json.loads(sidecar.read_text())['sessions']
        self.assertEqual(merged[0], dict(entries[0], session_id='new-id'))
        self.assertEqual(merged[1:3], entries[1:])
        self.assertCountEqual([item['session_id'] for item in merged[3:]], ['native-one', 'native-two'])
        self.assertFalse((self.root / 'other-save/assistant-sessions.json').exists())

    def test_failed_save_keeps_sidecars_and_invalidates_pending_team_save(self):
        self.start('agents')
        previous = b'{"sessions": [{"tool": "codex", "session_id": "old"}]}'
        assistant = self.save_dir / 'assistant-sessions.json'
        assistant.write_bytes(previous)
        team = self.save_dir / 'workbench-team-sessions.json'
        team.write_bytes(b'previous team sidecar')
        trae = self.save_dir / 'workbench-trae-sessions.json'
        trae.write_bytes(b'previous trae sidecar')
        self.tmux('set-option', '-g', '@workbench_resurrect_pending_snapshot', 'pending')
        bad_jq = self.shim / 'jq'
        bad_jq.write_text('#!/bin/sh\nexit 1\n')
        bad_jq.chmod(0o755)
        result = subprocess.run([str(REPO / 'bin/workbench-resurrect-save-all')], env=self.env,
                                capture_output=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(assistant.read_bytes(), previous)
        self.assertEqual(team.read_bytes(), b'previous team sidecar')
        self.assertEqual(trae.read_bytes(), b'previous trae sidecar')
        self.assertEqual(self.tmux('show-option', '-gqv', '@workbench_resurrect_pending_snapshot'), '')

    def test_manual_save_is_explicit_and_an_incomplete_full_save_fails(self):
        self.start('agents')
        fake_core = self.shim / 'fake-core'
        fake_core.write_text('#!/bin/sh\nprintf \'{"agents":[]}\\n\'\n')
        fake_core.chmod(0o755)
        self.env['TMUX_AGENT_WORKBENCH_BIN'] = str(fake_core)
        manual = subprocess.run([str(REPO / 'bin/tmux-agent-workbench-cli'), 'resurrect', 'save'],
                                env=self.env, capture_output=True, timeout=30)
        self.assertEqual(manual.returncode, 0, manual.stderr.decode())
        self.assertIn('assistant metadata only', manual.stderr.decode())
        self.assertTrue((self.save_dir / 'assistant-sessions.json').is_file())
        self.assertFalse((self.save_dir / 'workbench-team-sessions.json').exists())
        complete = subprocess.run([str(REPO / 'bin/workbench-resurrect-save-all')], env=self.env,
                                  capture_output=True, timeout=30)
        self.assertNotEqual(complete.returncode, 0)
        self.assertIn('requires a complete tmux-resurrect save', complete.stderr.decode())
        self.assertFalse((self.save_dir / 'workbench-team-sessions.json').exists())
