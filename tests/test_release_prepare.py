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
                              REPAIR_RELEASE_TAG='', REPAIR_ISH_ONLY='')
        self.env.start()
        self.addCleanup(self.env.stop)

    def git(self, *args):
        return subprocess.check_output(['git', *args], text=True).strip()

    def prepare(self):
        self.event.write_text(json.dumps({'workflow_run': self.workflow}))
        return release.prepare()

    @staticmethod
    def selected_targets(result):
        return json.loads(result['rust_matrix'])['include']

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
        self.assertEqual(result['full_matrix'], 'true')
        self.assertEqual(result['rust_changed'], 'true')
        self.assertEqual(result['ish_target'], 'true')
        self.assertEqual(result['windows_companion'], 'true')
        self.assertEqual(result['windows_arm64'], 'true')
        self.assertEqual(result['required_assets'], '5')
        self.assertEqual(len(self.selected_targets(result)), 8)
        self.assertEqual(self.git('rev-parse', 'v1.2.3^{commit}'), self.head)
        self.assertIn(self.head, self.git('ls-remote', 'origin', 'refs/tags/v1.2.3^{}'))
        self.assertEqual(self.prepare(), {'enabled': 'false'})

    def test_prerelease_uses_compact_build_matrix(self):
        self.git('tag', '-a', 'v1.2.2', self.head, '-m', 'previous release')
        Path('Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.2.4-beta.1"\n')
        self.git('add', 'Cargo.toml')
        self.git('commit', '-qm', 'prerelease source')
        head = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', head)
        self.workflow['head_sha'] = head

        result = self.prepare()

        self.assertEqual(result['tag'], 'v1.2.4-beta.1')
        self.assertEqual(result['full_matrix'], 'false')
        self.assertEqual(result['rust_changed'], 'true')
        self.assertEqual(result['ish_target'], 'false')
        self.assertEqual(result['windows_companion'], 'false')
        self.assertEqual(result['windows_arm64'], 'false')
        self.assertEqual(result['required_assets'], '3')
        self.assertEqual({target['target'] for target in self.selected_targets(result)}, {
            'aarch64-linux-android', 'x86_64-unknown-linux-gnu',
            'aarch64-apple-darwin',
        })

    def test_ish_compatibility_change_adds_only_i586_archive(self):
        self.git('tag', '-a', 'v1.2.3', self.head, '-m', 'previous release')
        Path('Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.2.4-beta.1"\n')
        Path('src').mkdir()
        Path('src/compat.rs').write_text('// iSH compatibility change\n')
        self.git('add', 'Cargo.toml', 'src/compat.rs')
        self.git('commit', '-qm', 'update iSH compatibility')
        head = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', head)
        self.workflow['head_sha'] = head

        result = self.prepare()

        self.assertEqual(result['rust_changed'], 'true')
        self.assertEqual(result['ish_target'], 'true')
        self.assertEqual(result['windows_companion'], 'false')
        self.assertEqual(result['required_assets'], '3')
        selected = {target['target']: target for target in self.selected_targets(result)}
        self.assertEqual(len(selected), 4)
        self.assertEqual(selected['i586-unknown-linux-musl']['priority'], 'low')

    def test_windows_change_adds_only_x64_client_artifacts(self):
        self.git('tag', '-a', 'v1.2.3', self.head, '-m', 'previous release')
        Path('Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.2.4-beta.1"\n')
        Path('windows-companion').mkdir()
        Path('windows-companion/wb-client.csproj').write_text('<Project />\n')
        self.git('add', 'Cargo.toml', 'windows-companion/wb-client.csproj')
        self.git('commit', '-qm', 'update Windows companion')
        head = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', head)
        self.workflow['head_sha'] = head

        result = self.prepare()

        self.assertEqual(result['rust_changed'], 'true')
        self.assertEqual(result['ish_target'], 'false')
        self.assertEqual(result['windows_companion'], 'true')
        self.assertEqual(result['windows_arm64'], 'false')
        self.assertEqual(result['required_assets'], '5')
        self.assertEqual(len(self.selected_targets(result)), 3)

    def test_stable_windows_arm64_is_optional_and_only_for_changed_client(self):
        self.git('tag', '-a', 'v1.2.2', self.head, '-m', 'previous release')
        Path('windows-companion').mkdir()
        Path('windows-companion/Program.cs').write_text('// Windows client change\n')
        self.git('add', 'windows-companion/Program.cs')
        self.git('commit', '-qm', 'update Windows client')
        head = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', head)
        self.workflow['head_sha'] = head

        result = self.prepare()

        self.assertEqual(result['full_matrix'], 'true')
        self.assertEqual(result['rust_changed'], 'false')
        self.assertEqual(result['windows_companion'], 'true')
        self.assertEqual(result['windows_arm64'], 'true')
        self.assertEqual(result['required_assets'], '2')
        self.assertEqual(len(self.selected_targets(result)), 1)

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

    def test_ish_repair_adds_only_the_missing_asset_to_a_slim_prerelease(self):
        Path('src').mkdir()
        Path('src/compat.rs').write_text('// iSH compatibility\n')
        self.git('add', 'src/compat.rs')
        self.git('commit', '-qm', 'add iSH compatibility')
        self.git('tag', 'v1.2.3')
        Path('Cargo.toml').write_text('[package]\nname="fixture"\nversion="1.2.4-beta.1"\n')
        self.git('add', 'Cargo.toml')
        self.git('commit', '-qm', 'prerelease source')
        source = self.git('rev-parse', 'HEAD')
        self.git('update-ref', 'refs/remotes/origin/main', source)
        self.workflow['head_sha'] = source

        original = self.prepare()
        self.assertEqual(original['ish_target'], 'false')

        with patch.dict(os.environ, REPAIR_RELEASE_TAG=original['tag'], REPAIR_ISH_ONLY='true'):
            result = self.prepare()

        self.assertEqual(result['source'], source)
        self.assertEqual(result['repair_ish_only'], 'true')
        self.assertEqual(result['required_assets'], '1')
        self.assertEqual(result['windows_companion'], 'false')
        self.assertEqual(self.selected_targets(result), [{
            'os': 'ubuntu-24.04', 'target': 'i586-unknown-linux-musl',
            'android': False, 'priority': 'high',
        }])


if __name__ == '__main__':
    unittest.main()
