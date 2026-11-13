#!/usr/bin/env python3
"""Exercise team resurrection on a private tmux server without model calls."""
import json
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import shlex
import sys
import tarfile
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
HELPER = REPO / 'bin/workbench-resurrect-team'
HISTORY_STATE = Path(os.environ.get('WORKBENCH_TEST_TEAM_HISTORY_STATE') or REPO / 'tests/fixtures/team-history.json')


class TeamResurrection(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='wb-team-restore-')
        self.root = Path(self.tmp.name)
        self.socket = str(self.root / 'tmux.sock')
        self.env = dict(os.environ, TMUX_AGENT_WORKBENCH_TMUX_SOCKET=self.socket,
                        XDG_STATE_HOME=str(self.root / 'state'),
                        XDG_DATA_HOME=str(self.root / 'data'))
        self.env.pop('TMUX', None)
        self.env.pop('TMUX_PANE', None)
        self.directory = self.root / 'state/agent-team/test-team'
        self.directory.mkdir(parents=True)
        self.state_path = self.directory / 'state.json'
        (self.directory / 'ready').touch()
        self.start()
        lead = self.tmux('display-message', '-p', '-t', 'team:0.0', '#{pane_id}')
        worker = self.tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', lead)
        window = self.tmux('display-message', '-p', '-t', lead, '#{window_id}')
        for pane, role in ((lead, 'lead'), (worker, 'worker')):
            self.tmux('set-option', '-p', '-t', pane, '@agent_team', 'test-team')
            self.tmux('set-option', '-p', '-t', pane, '@agent_team_role', role)
        self.state = {'id': 'test-team', 'socket': self.socket, 'window': window,
                      'tool': 'codex', 'cwd': str(self.root), 'budget': False, 'extra': [],
                      'config': {'limit': 2, 'yolo': True, 'worker_args': []},
                      'members': {'lead': {'pane': lead, 'status': 'running', 'cursor': 1},
                                  'worker_a': {'pane': worker, 'status': 'running', 'cursor': 0,
                                               'assignment': {'status': 'running'}}},
                      'messages': [{'id': 1, 'to': 'worker_a', 'text': 'pending'}]}
        self.state_path.write_text(json.dumps(self.state))
        (self.root / 'save').mkdir(parents=True, exist_ok=True)
        (self.root / 'save/last').write_text('snapshot for team')
        entries = [{'pane': 'team:0.' + str(index), 'tool': 'codex',
                    'session_id': 'saved-' + str(index), 'session_name': 'team',
                    'window_index': '0', 'pane_index': str(index)} for index in range(2)]
        (self.root / 'save/assistant-sessions.json').write_text(json.dumps({'sessions': entries}))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')

    def tearDown(self):
        subprocess.run(['tmux', '-S', self.socket, 'kill-server'], env=self.env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.tmp.cleanup()

    def tmux(self, *args):
        return subprocess.check_output(['tmux', '-S', self.socket, *args], env=self.env, text=True).strip()

    def helper(self, *args, **kw):
        return subprocess.run([str(HELPER), *args], env=self.env, check=True, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kw)

    def start(self):
        self.tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'team', '-n', 'Task', '-x', '120', '-y', '40')
        self.tmux('set-option', '-g', 'default-shell', '/bin/sh')
        self.tmux('set-option', '-g', '@resurrect-dir', str(self.root / 'save'))

    def restart(self):
        self.tmux('kill-server')
        self.tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'bootstrap')
        self.tmux('set-option', '-g', 'default-shell', '/bin/sh')
        self.tmux('set-option', '-g', '@resurrect-dir', str(self.root / 'save'))
        self.helper('pre-restore')
        self.tmux('new-session', '-d', '-s', 'team', '-n', 'Task', '-x', '120', '-y', '40')
        self.tmux('split-window', '-d', '-t', 'team:0.0')

    def test_rebind_preserves_messages_and_member_environment(self):
        self.helper('restore')
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)
        self.restart()
        # The old lead can mark the state closed while its tmux server exits.
        state = json.loads(self.state_path.read_text())
        state['closed'] = True
        self.state_path.write_text(json.dumps(state))
        self.helper('restore')
        state = json.loads(self.state_path.read_text())
        self.assertNotIn('closed', state)
        self.assertEqual(state['messages'], self.state['messages'])
        self.assertEqual(state['members']['worker_a']['assignment'], {'status': 'running'})
        worker = state['members']['worker_a']['pane']
        lead = state['members']['lead']['pane']
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', worker, '@agent_team_member'), 'worker_a')
        self.assertEqual(self.tmux('show-option', '-wqv', '-t', state['window'], '@workbench_restored_team'), 'test-team')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', lead, '@agent_team_member'), 'lead')

    def test_stopped_member_is_not_reactivated_even_if_saved_active(self):
        state = json.loads(self.state_path.read_text())
        state['members']['worker_a']['status'] = 'stopped'
        for index in range(25):
            state['members']['old_' + str(index)] = {'pane': '', 'status': 'stopped'}
        self.state_path.write_text(json.dumps(state))
        self.restart()
        self.helper('restore')
        state = json.loads(self.state_path.read_text())
        self.assertEqual(state['members']['worker_a']['status'], 'stopped')
        self.assertEqual(state['members']['worker_a']['pane'], '')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team_member'), 'lead')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@agent_team_member'), '')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), 'test-team')
        self.helper('finish-restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), '')

    def test_saved_ready_status_survives_shutdown_exit(self):
        state = json.loads(self.state_path.read_text())
        state['members']['worker_a']['status'] = 'ready'
        self.state_path.write_text(json.dumps(state))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        sidecar = json.loads((self.root / 'save/workbench-team-sessions.json').read_text())
        self.assertEqual(next(m['status'] for m in sidecar['teams'][0]['members'] if m['member'] == 'worker_a'), 'ready')
        state['members']['lead']['status'] = 'exited'
        state['members']['worker_a']['status'] = 'exited'
        state['closed'] = True
        self.state_path.write_text(json.dumps(state))
        self.restart()
        self.helper('restore')
        state = json.loads(self.state_path.read_text())
        self.assertEqual(state['members']['lead']['status'], 'running')
        self.assertEqual(state['members']['worker_a']['status'], 'ready')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@agent_team_member'), 'worker_a')

    def test_ambiguous_session_is_left_unconfirmed(self):
        assistant = self.root / 'save/assistant-sessions.json'
        data = json.loads(assistant.read_text())
        data['sessions'].append(dict(data['sessions'][1], session_id='other-session'))
        assistant.write_text(json.dumps(data))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        self.helper('restore')
        state = json.loads(self.state_path.read_text())
        self.assertEqual(state['members']['worker_a']['pane'], '')
        self.assertEqual(state['members']['worker_a']['status'], 'running')
        self.assertEqual(state['members']['worker_a']['restore_status'], 'unconfirmed')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), 'test-team')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team_member'), 'lead')

    def test_duplicate_session_id_blocks_the_team(self):
        assistant = self.root / 'save/assistant-sessions.json'
        data = json.loads(assistant.read_text())
        data['sessions'][1]['session_id'] = data['sessions'][0]['session_id']
        assistant.write_text(json.dumps(data))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        self.helper('restore')
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)
        for pane in ('team:0.0', 'team:0.1'):
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@workbench_team_resume_blocked'), 'test-team')
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@agent_team'), '')

    def test_missing_session_is_unconfirmed_and_not_launched(self):
        assistant = self.root / 'save/assistant-sessions.json'
        data = json.loads(assistant.read_text())
        data['sessions'].pop()
        assistant.write_text(json.dumps(data))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        self.helper('restore')
        state = json.loads(self.state_path.read_text())
        self.assertEqual(state['members']['worker_a']['pane'], '')
        self.assertEqual(state['members']['worker_a']['restore_status'], 'unconfirmed')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), 'test-team')
        if os.environ.get('WORKBENCH_TEST_AGENT_TEAM_CLI'):
            env = dict(self.env, AGENT_TEAM_DIR=str(self.directory), AGENT_TEAM_ROLE='lead')
            status = json.loads(subprocess.check_output([os.environ['WORKBENCH_TEST_AGENT_TEAM_CLI'], 'status'],
                                                        env=env, text=True, timeout=10))
            self.assertFalse(status['worker_a']['alive'])
            self.assertEqual(status['worker_a']['restore_status'], 'unconfirmed')

    def test_stopped_member_without_session_stays_stopped_and_unconfirmed(self):
        state = json.loads(self.state_path.read_text())
        state['members']['worker_a']['status'] = 'stopped'
        self.state_path.write_text(json.dumps(state))
        assistant = self.root / 'save/assistant-sessions.json'
        data = json.loads(assistant.read_text())
        data['sessions'].pop()
        assistant.write_text(json.dumps(data))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        self.helper('restore')
        member = json.loads(self.state_path.read_text())['members']['worker_a']
        self.assertEqual((member['status'], member['pane'], member['restore_status']),
                         ('stopped', '', 'unconfirmed'))

    @unittest.skipUnless(HISTORY_STATE.is_file() and os.environ.get('WORKBENCH_TEST_AGENT_TEAM_CLI'),
                         'historical fixture or agent-team CLI not configured')
    def test_historical_state_copy_keeps_stopped_members(self):
        source = HISTORY_STATE
        original = source.read_bytes()
        state = json.loads(original)
        state['id'] = 'test-team'
        state['socket'] = self.socket
        state['window'] = self.state['window']
        state['cwd'] = str(self.root)
        state['members']['lead']['pane'] = self.state['members']['lead']['pane']
        state['members']['v2_merge_review']['pane'] = self.state['members']['worker_a']['pane']
        self.state_path.write_text(json.dumps(state))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        self.helper('restore')
        restored = json.loads(self.state_path.read_text())
        self.assertEqual(restored['members']['v2_merge_review']['status'], 'stopped')
        self.assertEqual(restored['members']['v2_merge_review']['pane'], '')
        self.assertEqual(len(restored['messages']), len(state['messages']))
        self.assertEqual(len(restored['members']), len(state['members']))
        for name, member in state['members'].items():
            if name != 'lead':
                self.assertEqual(restored['members'][name]['status'], member['status'])
        self.assertEqual(sum(item['pane'] == '' for item in restored['members'].values()), len(state['members']) - 1)
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team_member'), 'lead')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), 'test-team')
        cli = os.environ['WORKBENCH_TEST_AGENT_TEAM_CLI']
        env = dict(self.env, AGENT_TEAM_DIR=str(self.directory), AGENT_TEAM_ROLE='lead')
        status = json.loads(subprocess.check_output([cli, 'status'], env=env, text=True, timeout=15))
        self.assertTrue(status['lead']['alive'])
        self.assertTrue(all(not item['alive'] for name, item in status.items() if name != 'lead'))
        self.assertEqual(hashlib.sha256(source.read_bytes()).digest(), hashlib.sha256(original).digest())

    def test_refuses_partial_or_already_owned_targets(self):
        self.restart()
        worker = self.tmux('display-message', '-p', '-t', 'team:0.1', '#{pane_id}')
        self.tmux('set-option', '-p', '-t', worker, '@agent_team', 'other-team')
        self.helper('restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), '')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', worker, '@agent_team'), 'other-team')
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)

    def test_missing_state_blocks_generic_assistant_relaunch(self):
        self.restart()
        self.state_path.unlink()
        self.helper('restore')
        for pane in ('team:0.0', 'team:0.1'):
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@agent_team'), '')
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@workbench_team_resume_blocked'), 'test-team')

    def test_save_rejects_missing_or_ambiguous_team_state(self):
        sidecar = self.root / 'save/workbench-team-sessions.json'
        original = sidecar.read_bytes()
        snapshot = self.root / 'save/last'
        for case in ('missing', 'unmapped', 'duplicate', 'invalid-marker'):
            with self.subTest(case=case):
                state = json.loads(json.dumps(self.state))
                if case == 'unmapped':
                    del state['members']['worker_a']
                if case == 'duplicate':
                    state['members']['worker_duplicate'] = dict(state['members']['worker_a'])
                self.state_path.write_text(json.dumps(state))
                if case == 'missing':
                    self.state_path.unlink()
                self.tmux('set-option', '-p', '-t', 'team:0.1', '@agent_team',
                          'invalid/team' if case == 'invalid-marker' else 'test-team')
                snapshot.write_text('snapshot-' + case + '\nworkbench-assistant-sidecars\t1\n')
                self.helper('stage', str(snapshot))
                result = subprocess.run([str(HELPER), 'save'], env=self.env, text=True, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('team', result.stderr)
                self.assertEqual(sidecar.read_bytes(), original)
                digest = hashlib.sha256(snapshot.read_bytes()).hexdigest()
                self.assertFalse((self.root / 'save/workbench-snapshot-sidecars' / (digest + '.json')).exists())
                pre = subprocess.run([str(HELPER), 'pre-restore'], env=self.env, capture_output=True)
                self.assertNotEqual(pre.returncode, 0)
                restore = subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')],
                                         env=self.env, capture_output=True, timeout=20)
                self.assertNotEqual(restore.returncode, 0)

    def test_missing_target_blocks_other_fresh_team_panes(self):
        self.restart()
        self.tmux('kill-pane', '-t', 'team:0.1')
        self.helper('restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), '')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@workbench_team_resume_blocked'), 'test-team')
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)

    def test_same_server_restore_does_not_take_existing_panes(self):
        self.tmux('new-session', '-d', '-s', 'bootstrap')
        self.tmux('kill-session', '-t', 'team')
        self.helper('pre-restore')
        self.tmux('new-session', '-d', '-s', 'team', '-n', 'Task', '-x', '120', '-y', '40')
        self.tmux('split-window', '-d', '-t', 'team:0.0')
        self.helper('restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), 'test-team')
        # Repeated restore must not adopt or overwrite pre-existing panes.
        self.tmux('set-option', '-pu', '-t', 'team:0.0', '@agent_team')
        self.tmux('set-option', '-pu', '-t', 'team:0.1', '@agent_team')
        self.helper('pre-restore')
        self.helper('restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), '')

    def test_new_snapshot_refuses_to_take_over_existing_team_panes(self):
        snapshot = self.root / 'save/last'
        snapshot.write_text('snapshot\nworkbench-assistant-sidecars\t1\n')
        self.helper('stage', str(snapshot))
        self.helper('save')
        self.helper('pre-restore')
        result = subprocess.run([str(HELPER), 'restore'], env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('existing pane', result.stderr)
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)

    def test_snapshot_mismatch_does_not_claim_panes(self):
        self.restart()
        (self.root / 'save/last').write_text('a different historical snapshot')
        self.helper('restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), '')
        for pane in ('team:0.0', 'team:0.1'):
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@workbench_team_resume_blocked'), 'test-team')
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)

    def test_mismatched_team_snapshot_never_falls_back_to_solo(self):
        self.restart()
        (self.root / 'save/last').write_text('a different historical snapshot')
        shim = self.root / 'bin'
        shim.mkdir()
        launched = self.root / 'unexpected-launch'
        codex = shim / 'codex'
        codex.write_text('#!/bin/sh\nprintf launched > ' + shlex.quote(str(launched)) + '\n')
        codex.chmod(0o755)
        path = str(shim) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
        self.env['PATH'] = path
        self.tmux('set-environment', '-g', 'PATH', path)
        for pane in ('team:0.0', 'team:0.1'):
            self.tmux('respawn-pane', '-k', '-t', pane, '/bin/sh')
        result = subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')],
                                env=dict(self.env, TMUX=self.socket + ',1,0'),
                                capture_output=True, timeout=25)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        time.sleep(.2)
        self.assertFalse(launched.exists())
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)

    def test_missing_team_sidecar_consumes_restore_inventory(self):
        self.helper('pre-restore')
        (self.root / 'save/workbench-team-sessions.json').unlink()
        self.helper('restore')
        self.assertFalse((self.root / 'save/workbench-team-restore-inventory.json').exists())

    def test_assistant_only_save_cannot_rebind_an_older_layout(self):
        sidecar = self.root / 'save/workbench-team-sessions.json'
        original = sidecar.read_bytes()
        (self.root / 'save/assistant-sessions.json').write_text('{"sessions": []}')
        with self.assertRaises(subprocess.CalledProcessError):
            self.helper('save')
        self.assertEqual(sidecar.read_bytes(), original)
        self.restart()
        self.helper('restore')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), '')
        for pane in ('team:0.0', 'team:0.1'):
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@workbench_team_resume_blocked'), 'test-team')

    def test_pre_restore_preserves_existing_hook(self):
        prior = self.root / 'prior hook'
        output = self.root / 'prior-ran'
        prior.write_text('#!/bin/sh\nprintf done > ' + shlex.quote(str(output)) + '\n')
        prior.chmod(0o755)
        self.tmux('set-option', '-g', '@workbench-resurrect-prior-pre-restore-all', shlex.quote(str(prior)))
        subprocess.run([str(REPO / 'bin/workbench-resurrect-pre-restore')], env=self.env, check=True)
        self.assertEqual(output.read_text(), 'done')
        before = json.loads((self.root / 'save/workbench-team-restore-inventory.json').read_text())
        self.assertIn(self.state['members']['lead']['pane'], before['panes'])

    def test_sidebar_gap_is_mapped_in_snapshot_and_sidecars(self):
        self.tmux('kill-pane', '-t', self.state['members']['worker_a']['pane'])
        lead = self.state['members']['lead']['pane']
        sidebar = self.tmux('split-window', '-d', '-b', '-h', '-P', '-F', '#{pane_id}', '-t', lead)
        self.tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        worker = self.tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', lead)
        self.tmux('set-option', '-p', '-t', worker, '@agent_team', 'test-team')
        self.state['members']['worker_a']['pane'] = worker
        self.state_path.write_text(json.dumps(self.state))
        entries = [{'pane': 'team:0.2', 'session_name': 'team', 'window_index': '0',
                    'pane_index': '2', 'tool': 'codex', 'session_id': 'worker-session'}]
        assistant = self.root / 'save/assistant-sessions.json'
        assistant.write_text(json.dumps({'sessions': entries, 'relaunch': [{'pane': 'team:0.2'}]}))
        archive = self.root / 'save/pane_contents.tar.gz'
        with tarfile.open(archive, 'w:gz') as out:
            for index, content in enumerate((b'sidebar', b'lead', b'worker')):
                info = tarfile.TarInfo('./pane_contents/pane-team:0.' + str(index))
                info.size = len(content)
                out.addfile(info, io.BytesIO(content))
        panes = self.tmux('list-panes', '-t', 'team:0', '-F',
                          'pane\t#{session_name}\t#{window_index}\t0\t:-\t#{pane_index}\ttitle\t:/tmp\t0\tsh\t:')
        window = self.tmux('display-message', '-p', '-t', 'team:0', '-F',
                           'window\t#{session_name}\t#{window_index}\t:Team\t0\t:-\t#{window_layout}\toff')
        snapshot = self.root / 'snapshot'
        snapshot.write_text(panes + '\n' + window + '\n')
        self.env['TMUX_AGENT_WORKBENCH_BIN'] = str(REPO / 'target/debug/tmux-agent-workbench')
        subprocess.run([str(REPO / 'bin/workbench-resurrect-save-hook'), str(snapshot)],
                       env=self.env, check=True)
        (self.root / 'save/last').write_text(snapshot.read_text())
        self.helper('save')
        records = [line.split('\t') for line in snapshot.read_text().splitlines() if line.startswith('pane\t')]
        self.assertEqual([row[5] for row in records], ['0', '1'])
        self.assertEqual(json.loads(assistant.read_text())['sessions'][0]['pane'], 'team:0.1')
        self.assertEqual(json.loads(assistant.read_text())['relaunch'][0]['pane'], 'team:0.1')
        with tarfile.open(archive, 'r:gz') as saved:
            self.assertEqual({entry.name: saved.extractfile(entry).read() for entry in saved if entry.isfile()},
                             {'./pane_contents/pane-team:0.0': b'lead', './pane_contents/pane-team:0.1': b'worker'})
        teams = json.loads((self.root / 'save/workbench-team-sessions.json').read_text())['teams']
        self.assertEqual(next(m['pane'] for m in teams[0]['members'] if m['member'] == 'worker_a'), '1')

    @unittest.skipUnless(os.environ.get('WORKBENCH_TEST_RESURRECT_SCRIPTS'), 'tmux-resurrect scripts not configured')
    def test_real_resurrect_restores_team_after_sidebar_pruning(self):
        scripts = Path(os.environ['WORKBENCH_TEST_RESURRECT_SCRIPTS'])
        lead = self.state['members']['lead']['pane']
        worker = self.state['members']['worker_a']['pane']
        sidebar = self.tmux('split-window', '-d', '-b', '-h', '-P', '-F', '#{pane_id}', '-t', lead)
        self.tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        entries = []
        for pane, sid in ((lead, 'saved-lead'), (worker, 'saved-worker')):
            index = self.tmux('display-message', '-p', '-t', pane, '#{pane_index}')
            entries.append({'pane': 'team:0.' + index, 'session_name': 'team', 'window_index': '0',
                            'pane_index': index, 'tool': 'codex', 'session_id': sid})
        (self.root / 'save/assistant-sessions.json').write_text(json.dumps({'sessions': entries}))
        self.tmux('set-option', '-g', '@resurrect-hook-post-save-layout', shlex.quote(str(REPO / 'bin/workbench-resurrect-save-hook')))
        self.tmux('set-option', '-g', '@resurrect-hook-post-save-all', shlex.quote(str(HELPER)) + ' save')
        env = dict(self.env, TMUX=self.socket + ',1,0',
                   TMUX_AGENT_WORKBENCH_BIN=str(REPO / 'target/debug/tmux-agent-workbench'))
        subprocess.run([str(scripts / 'save.sh'), 'quiet'], env=env, check=True, capture_output=True)
        self.tmux('kill-server')
        shim = self.root / 'bin'
        shim.mkdir()
        # Restored login shells may reset PATH (for example in /etc/profile),
        # which can select an older tmux/Python or even the real assistant.
        # Keep all restored panes on this test's executables and fake CLI.
        (shim / 'tmux').symlink_to(shutil.which('tmux'))
        (shim / 'python3').symlink_to(sys.executable)
        log = self.root / 'real-launches'
        codex = shim / 'codex'
        codex.write_text('#!/bin/sh\nfor arg do last=$arg; done\nprintf "%s:%s\\n" "$AGENT_TEAM_ROLE" "$last" >> "$TEAM_TEST_OUTPUT"\nsleep 30\n')
        codex.chmod(0o755)
        team_cli = shim / 'agent-team'
        team_cli.symlink_to(os.environ['WORKBENCH_TEST_AGENT_TEAM_CLI'])
        self.env['PATH'] = str(shim) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
        self.env['TEAM_TEST_OUTPUT'] = str(log)
        self.tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'bootstrap', '-x', '120', '-y', '40')
        self.tmux('set-option', '-g', 'default-shell', '/bin/sh')
        self.tmux('set-option', '-g', 'default-command', '/bin/sh')
        self.tmux('set-option', '-g', '@resurrect-dir', str(self.root / 'save'))
        self.tmux('set-option', '-g', '@resurrect-hook-pre-restore-all', shlex.quote(str(REPO / 'bin/workbench-resurrect-pre-restore')))
        self.tmux('set-option', '-g', '@resurrect-hook-post-restore-all', shlex.quote(str(REPO / 'bin/workbench-resurrect-restore-all')))
        env.update(self.env)
        env['TMUX'] = self.socket + ',1,0'
        result = subprocess.run([str(scripts / 'restore.sh'), 'quiet'], env=env,
                                check=True, capture_output=True, timeout=40)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and (not log.exists() or len(log.read_text().splitlines()) < 2):
            time.sleep(.05)
        self.assertTrue(log.exists(), result.stderr.decode())
        self.assertCountEqual(log.read_text().splitlines(), ['lead:saved-lead', 'worker_a:saved-worker'])
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.0', '@agent_team'), 'test-team')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@agent_team'), 'test-team')
        for role in ('lead', 'worker_a'):
            doctor_env = dict(self.env, AGENT_TEAM_DIR=str(self.directory), AGENT_TEAM_ROLE=role)
            result = subprocess.check_output([str(team_cli), 'doctor'], env=doctor_env, text=True)
            self.assertEqual(json.loads(result)['role'], role)

    def test_composed_restore_relaunches_members_with_team_context(self):
        self.restart()
        shim = self.root / 'bin'
        shim.mkdir()
        output = self.root / 'launched'
        codex = shim / 'codex'
        codex.write_text('#!/bin/sh\nfor arg do last=$arg; done\nprintf "%s:%s:%s\\n" "$AGENT_TEAM_DIR" "$AGENT_TEAM_ROLE" "$last" >> "$TEAM_TEST_OUTPUT"\nsleep 30\n')
        codex.chmod(0o755)
        team_cli = shim / 'agent-team'
        actual_cli = os.environ.get('WORKBENCH_TEST_AGENT_TEAM_CLI')
        if actual_cli:
            team_cli.symlink_to(actual_cli)
        else:
            team_cli.write_text('#!/bin/sh\n[ "$1" = resume-pane ] || exit 1\nexport AGENT_TEAM_DIR="$2" AGENT_TEAM_ROLE="$3"\nexec codex resume "$4"\n')
            team_cli.chmod(0o755)
        path = str(shim) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
        self.env['PATH'] = path
        self.env['TEAM_TEST_OUTPUT'] = str(output)
        self.tmux('set-environment', '-g', 'PATH', path)
        self.tmux('set-environment', '-g', 'TEAM_TEST_OUTPUT', str(output))
        # The server was already started; new panes inherit its updated env.
        self.tmux('kill-pane', '-t', 'team:0.1')
        self.tmux('respawn-pane', '-k', '-t', 'team:0.0', '/bin/sh')
        self.tmux('split-window', '-d', '-t', 'team:0.0', '/bin/sh')
        entries = [{'pane': 'team:0.' + str(index), 'tool': 'codex',
                    'session_id': 'saved-' + str(index), 'session_name': 'team',
                    'window_index': '0', 'pane_index': str(index)} for index in range(2)]
        (self.root / 'save/assistant-sessions.json').write_text(json.dumps({'sessions': entries}))
        # Prevent the upstream hook's attach wait without opening a real client.
        real_tmux = subprocess.check_output(['which', 'tmux'], text=True).strip()
        tmux_shim = shim / 'tmux'
        tmux_shim.write_text('#!/bin/sh\ncase "$1:${3:-}" in list-clients:*|-S:list-clients) printf "client\\n"; exit 0 ;; esac\nexec ' + real_tmux + ' "$@"\n')
        tmux_shim.chmod(0o755)
        env = dict(self.env, PATH=path, TMUX=self.socket + ',1,0', TEAM_TEST_OUTPUT=str(output))
        result = subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')], env=env,
                                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and (not output.exists() or len(output.read_text().splitlines()) < 2):
            time.sleep(.05)
        if not output.exists():
            self.fail((result.stderr.decode(), self.tmux('capture-pane', '-p', '-t', 'team:0.0')))
        lines = output.read_text().splitlines()
        self.assertIn(str(self.directory) + ':lead:saved-0', lines)
        self.assertIn(str(self.directory) + ':worker_a:saved-1', lines)

    def test_composed_restore_never_relaunches_stopped_member(self):
        state = json.loads(self.state_path.read_text())
        state['members']['worker_a']['status'] = 'stopped'
        self.state_path.write_text(json.dumps(state))
        self.restart()
        shim = self.root / 'bin'
        shim.mkdir()
        output = self.root / 'launched'
        team_cli = shim / 'agent-team'
        team_cli.write_text('#!/bin/sh\nprintf "%s:%s\\n" "$3" "$4" >> "$TEAM_TEST_OUTPUT"\nsleep 30\n')
        team_cli.chmod(0o755)
        real_tmux = subprocess.check_output(['which', 'tmux'], text=True).strip()
        tmux_shim = shim / 'tmux'
        tmux_shim.write_text('#!/bin/sh\ncase "$1:${3:-}" in list-clients:*|-S:list-clients) printf "client\\n"; exit 0 ;; esac\nexec ' + real_tmux + ' "$@"\n')
        tmux_shim.chmod(0o755)
        path = str(shim) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
        self.env.update(PATH=path, TEAM_TEST_OUTPUT=str(output))
        self.tmux('set-environment', '-g', 'PATH', path)
        self.tmux('set-environment', '-g', 'TEAM_TEST_OUTPUT', str(output))
        self.tmux('kill-pane', '-t', 'team:0.1')
        self.tmux('respawn-pane', '-k', '-t', 'team:0.0', '/bin/sh')
        self.tmux('split-window', '-d', '-t', 'team:0.0', '/bin/sh')
        env = dict(self.env, TMUX=self.socket + ',1,0')
        result = subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')], env=env,
                                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and not output.exists():
            time.sleep(.05)
        self.assertTrue(output.exists(), result.stderr.decode())
        self.assertEqual(output.read_text().splitlines(), ['lead:saved-0'])
        self.assertIn('stopped or unconfirmed', result.stderr.decode())
        state = json.loads(self.state_path.read_text())
        self.assertEqual(state['members']['worker_a']['status'], 'stopped')
        self.assertEqual(state['members']['worker_a']['pane'], '')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), '')

    def test_composed_restore_never_launches_an_ambiguous_lead(self):
        assistant = self.root / 'save/assistant-sessions.json'
        data = json.loads(assistant.read_text())
        data['sessions'].append(dict(data['sessions'][0], session_id='other-lead'))
        assistant.write_text(json.dumps(data))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        shim = self.root / 'bin'
        shim.mkdir()
        output = self.root / 'unexpected-launch'
        for name in ('agent-team', 'codex'):
            executable = shim / name
            executable.write_text('#!/bin/sh\nprintf launched >> "$TEAM_TEST_OUTPUT"\nsleep 30\n')
            executable.chmod(0o755)
        path = str(shim) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
        self.env.update(PATH=path, TEAM_TEST_OUTPUT=str(output))
        self.tmux('set-environment', '-g', 'PATH', path)
        self.tmux('set-environment', '-g', 'TEAM_TEST_OUTPUT', str(output))
        self.tmux('kill-pane', '-t', 'team:0.1')
        self.tmux('respawn-pane', '-k', '-t', 'team:0.0', '/bin/sh')
        self.tmux('split-window', '-d', '-t', 'team:0.0', '/bin/sh')
        result = subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')],
                                env=dict(self.env, TMUX=self.socket + ',1,0'),
                                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        self.assertFalse(output.exists())
        self.assertIn('stopped or unconfirmed', result.stderr.decode())
        self.assertEqual(json.loads(self.state_path.read_text()), self.state)
        for pane in ('team:0.0', 'team:0.1'):
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@agent_team'), '')
            self.assertEqual(self.tmux('show-option', '-pqv', '-t', pane, '@workbench_team_resume_blocked'), '')

    def test_trae_restore_never_relaunches_stopped_member(self):
        state = json.loads(self.state_path.read_text())
        state['tool'] = 'traex'
        state['members']['worker_a']['status'] = 'stopped'
        self.state_path.write_text(json.dumps(state))
        (self.root / 'save/assistant-sessions.json').write_text(json.dumps({'sessions': []}))
        entries = [{'session_name': 'team', 'window_index': '0', 'pane_index': str(index),
                    'session_id': 'trae-' + str(index)} for index in range(2)]
        (self.root / 'save/workbench-trae-sessions.json').write_text(json.dumps(entries))
        self.helper('stage', str(self.root / 'save/last'))
        self.helper('save')
        self.restart()
        shim = self.root / 'bin'
        shim.mkdir()
        output = self.root / 'launched'
        team_cli = shim / 'agent-team'
        team_cli.write_text('#!/bin/sh\nprintf "%s:%s\\n" "$3" "$4" >> "$TEAM_TEST_OUTPUT"\nsleep 30\n')
        team_cli.chmod(0o755)
        real_tmux = subprocess.check_output(['which', 'tmux'], text=True).strip()
        tmux_shim = shim / 'tmux'
        tmux_shim.write_text('#!/bin/sh\ncase "$1:${3:-}" in list-clients:*|-S:list-clients) printf "client\\n"; exit 0 ;; esac\nexec ' + real_tmux + ' "$@"\n')
        tmux_shim.chmod(0o755)
        path = str(shim) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
        self.env.update(PATH=path, TEAM_TEST_OUTPUT=str(output))
        self.tmux('set-environment', '-g', 'PATH', path)
        self.tmux('set-environment', '-g', 'TEAM_TEST_OUTPUT', str(output))
        self.tmux('kill-pane', '-t', 'team:0.1')
        self.tmux('respawn-pane', '-k', '-t', 'team:0.0', '/bin/sh')
        self.tmux('split-window', '-d', '-t', 'team:0.0', '/bin/sh')
        env = dict(self.env, TMUX=self.socket + ',1,0')
        subprocess.run([str(REPO / 'bin/workbench-resurrect-restore-all')], env=env,
                       check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and not output.exists():
            time.sleep(.05)
        self.assertTrue(output.exists())
        self.assertEqual(output.read_text().splitlines(), ['lead:trae-0'])
        state = json.loads(self.state_path.read_text())
        self.assertEqual(state['members']['worker_a']['status'], 'stopped')
        self.assertEqual(state['members']['worker_a']['pane'], '')
        self.assertEqual(self.tmux('show-option', '-pqv', '-t', 'team:0.1', '@workbench_team_resume_blocked'), '')


if __name__ == '__main__':
    unittest.main()
