#!/usr/bin/env python3
"""Release only CI-verified main commits; existing tags are immutable."""
import json
import os
from pathlib import Path
import subprocess
import tomllib

ISH_COMPATIBILITY_PATHS = {
    'src/compat.rs',
    'src/ipc.rs',
    'src/main.rs',
}
RUST_ARTIFACT_PATHS = {
    'Cargo.toml',
    'Cargo.lock',
    'build.rs',
    'README.md',
    'LICENSE',
    'docs/assistant-sessions.md',
}


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def changed_paths(source):
    previous = subprocess.run(
        ['git', 'describe', '--tags', '--abbrev=0', '--match', 'v[0-9]*', f'{source}^'],
        text=True, capture_output=True, check=False)
    if previous.returncode != 0:
        # On the first release there is no baseline to compare against. Build
        # every platform once rather than accidentally publishing no assets.
        return {'*'}
    base = previous.stdout.strip()
    return set(run('git', 'diff', '--name-only', f'{base}..{source}').splitlines())


def prepare():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    repository = os.environ['GITHUB_REPOSITORY']
    main = run('git', 'rev-parse', 'origin/main')
    if os.environ['GITHUB_EVENT_NAME'] == 'workflow_run':
        workflow = event['workflow_run']
        if (workflow['conclusion'] != 'success' or workflow['event'] != 'push'
                or workflow['head_branch'] != 'main'
                or workflow['head_repository']['full_name'] != repository
                or workflow['head_sha'] != main):
            return {'enabled': 'false'}
    else:
        # Manual recovery also needs a successful post-merge CI at current main.
        runs = json.loads(run('gh', 'api',
            f'repos/{repository}/actions/workflows/ci.yml/runs?branch=main&event=push&status=success&head_sha={main}'))
        if not any(item['head_sha'] == main for item in runs['workflow_runs']):
            raise RuntimeError('Current main must pass CI before release recovery')
    repair = os.environ.get('REPAIR_RELEASE_TAG', '')
    if repair:
        subprocess.run(['git', 'check-ref-format', f'refs/tags/{repair}'], check=True)
        source = run('git', 'rev-parse', f'refs/tags/{repair}^{{commit}}')
        version = tomllib.loads(run('git', 'show', f'{source}:Cargo.toml'))['package']['version']
        if repair != f'v{version}':
            raise RuntimeError('Recovery tag must match its Cargo package version')
        subprocess.run(['git', 'merge-base', '--is-ancestor', source, main], check=True)
        tag = repair
    else:
        version = tomllib.loads(run('git', 'show', f'{main}:Cargo.toml'))['package']['version']
        tag = f'v{version}'
        if subprocess.run(['git', 'show-ref', '--verify', '--quiet', f'refs/tags/{tag}']).returncode == 0:
            print(f'{tag} already exists; no version bump to release')
            return {'enabled': 'false'}
        subprocess.run(['git', '-c', 'user.name=github-actions[bot]', '-c',
            'user.email=41898282+github-actions[bot]@users.noreply.github.com',
            'tag', '-a', tag, main, '-m', f'Release {tag}'], check=True)
        subprocess.run(['git', 'push', 'origin', f'refs/tags/{tag}'], check=True)
        source = main
    full_matrix = '-' not in version
    paths = changed_paths(source)
    rust_changed = '*' in paths or bool(paths & RUST_ARTIFACT_PATHS) or any(
        path.startswith(('src/', 'LICENSES/', '.cargo/')) for path in paths)
    ish_target = '*' in paths or bool(paths & ISH_COMPATIBILITY_PATHS)
    windows_companion = '*' in paths or any(
        path.startswith('windows-companion/') for path in paths)
    windows_arm64 = full_matrix and windows_companion
    required_assets = (3 if rust_changed else 0) + (2 if windows_companion else 0)
    return {
        'enabled': 'true',
        'tag': tag,
        'source': source,
        'validation': main,
        'full_matrix': str(full_matrix).lower(),
        'rust_changed': str(rust_changed).lower(),
        'ish_target': str(ish_target).lower(),
        'windows_companion': str(windows_companion).lower(),
        'windows_arm64': str(windows_arm64).lower(),
        'required_assets': str(required_assets),
    }


if __name__ == '__main__':
    outputs = prepare()
    with open(os.environ['GITHUB_OUTPUT'], 'a') as handle:
        for key, value in outputs.items():
            handle.write(f'{key}={value}\n')
