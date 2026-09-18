"""Exercise release gating against a real disposable git repository."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'prepare_release', Path(__file__).resolve().parents[1] / '.github/scripts/prepare-release.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseGateTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.old_cwd = Path.cwd()
        self.addCleanup(os.chdir, self.old_cwd)
        os.chdir(self.root)
        self.git('init', '-q')
        self.git('config', 'user.name', 'test')
        self.git('config', 'user.email', 'test@example.invalid')
        Path('Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.2.3"\n')
        self.git('add', 'Cargo.toml')
        self.git('commit', '-qm', 'release source')
        self.head = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', self.head)
        subprocess.run(['git', 'init', '--bare', '-q', str(self.root / 'remote')], check=True)
        self.git('remote', 'add', 'origin', str(self.root / 'remote'))
        self.event = self.root / 'event.json'
        self.workflow = dict(conclusion='success', event='push', head_branch='main',
                             head_repository={'full_name': 'owner/repo'}, head_sha=self.head)
        self.env = patch.dict(os.environ, GITHUB_EVENT_PATH=str(self.event),
                              GITHUB_REPOSITORY='owner/repo', GITHUB_EVENT_NAME='workflow_run',
                              REPAIR_RELEASE_TAG='')
        self.env.start()
        self.addCleanup(self.env.stop)

    def git(self, *args):
        return subprocess.check_output(['git', *args], text=True).strip()

    def prepare(self):
        self.event.write_text(json.dumps({'workflow_run': self.workflow}))
        return release.prepare()

    def test_only_successful_current_main_push_can_tag(self):
        cases = [('conclusion', 'failure'), ('event', 'pull_request'),
                 ('head_branch', 'feature'), ('head_sha', '0' * 40),
                 ('head_repository', {'full_name': 'fork/repo'})]
        for key, value in cases:
            with self.subTest(key=key):
                old = self.workflow[key]
                self.workflow[key] = value
                self.assertEqual(self.prepare(), {'enabled': 'false'})
                self.workflow[key] = old
        self.assertEqual(self.git('tag'), '')

    def test_tags_verified_commit_once(self):
        result = self.prepare()
        self.assertEqual(result['source'], self.head)
        self.assertEqual(result['tag'], 'v1.2.3')
        self.assertEqual(self.git('rev-parse', 'v1.2.3^{commit}'), self.head)
        self.assertIn(self.head, self.git('ls-remote', 'origin', 'refs/tags/v1.2.3^{}'))
        self.assertEqual(self.prepare(), {'enabled': 'false'})

    def test_repair_uses_original_tag_not_new_main(self):
        self.git('tag', 'v1.2.3')
        Path('new-code').write_text('new implementation')
        self.git('add', 'new-code')
        self.git('commit', '-qm', 'later main')
        current = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', current)
        self.workflow['head_sha'] = current
        with patch.dict(os.environ, REPAIR_RELEASE_TAG='v1.2.3'):
            result = self.prepare()
        self.assertEqual(result['source'], self.head)
        self.assertEqual(result['validation'], current)

    def test_manual_recovery_requires_successful_main_ci(self):
        original = release.run

        def fake_run(*args):
            if args[0] == 'gh':
                return '{"workflow_runs": []}'
            return original(*args)

        with patch.dict(os.environ, GITHUB_EVENT_NAME='workflow_dispatch'), patch.object(release, 'run', fake_run):
            with self.assertRaisesRegex(RuntimeError, 'must pass CI'):
                self.prepare()


if __name__ == '__main__':
    unittest.main()
