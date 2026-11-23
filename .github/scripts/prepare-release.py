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
RUST_TARGETS = [
    {'os': 'macos-15-intel', 'target': 'x86_64-apple-darwin',
     'android': False, 'priority': 'low'},
    {'os': 'macos-15', 'target': 'aarch64-apple-darwin',
     'android': False, 'priority': 'high'},
    {'os': 'ubuntu-24.04', 'target': 'x86_64-unknown-linux-gnu',
     'android': False, 'priority': 'high'},
    {'os': 'ubuntu-24.04', 'target': 'x86_64-unknown-linux-musl',
     'android': False, 'priority': 'low'},
    {'os': 'ubuntu-24.04', 'target': 'i586-unknown-linux-musl',
     'android': False, 'priority': 'low'},
    {'os': 'ubuntu-24.04-arm', 'target': 'aarch64-unknown-linux-gnu',
     'android': False, 'priority': 'low'},
    {'os': 'ubuntu-24.04', 'target': 'aarch64-linux-android',
     'android': True, 'android_linker': 'aarch64-linux-android24-clang',
     'priority': 'high'},
    {'os': 'ubuntu-24.04', 'target': 'x86_64-linux-android',
     'android': True, 'android_linker': 'x86_64-linux-android24-clang',
     'priority': 'low'},
]
STANDARD_TARGETS = {
    'aarch64-apple-darwin',
    'x86_64-unknown-linux-gnu',
    'aarch64-linux-android',
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


def rust_target_matrix(full_matrix, rust_changed, ish_target):
    if not rust_changed:
        targets = [target for target in RUST_TARGETS
                   if target['target'] == 'x86_64-unknown-linux-gnu']
    elif full_matrix:
        targets = RUST_TARGETS
    else:
        targets = [target for target in RUST_TARGETS
                   if target['target'] in STANDARD_TARGETS]
        if ish_target:
            targets.extend(target for target in RUST_TARGETS
                           if target['target'] == 'i586-unknown-linux-musl')
    return {'include': targets}


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
    repair_ish_only = os.environ.get('REPAIR_ISH_ONLY') == 'true'
    if repair_ish_only and not repair:
        raise RuntimeError('An existing release tag is required for an iSH asset repair')
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
    rust_matrix = rust_target_matrix(full_matrix, rust_changed, ish_target)
    if repair_ish_only:
        # Do not ship iSH binaries for tags from before the runtime supported it.
        if subprocess.run(['git', 'cat-file', '-e', f'{source}:src/compat.rs']).returncode:
            raise RuntimeError(f'{tag} does not contain iSH runtime compatibility')
        full_matrix = windows_companion = windows_arm64 = False
        rust_changed = ish_target = True
        required_assets = 1
        rust_matrix = {'include': [
            dict(target, priority='high') for target in RUST_TARGETS
            if target['target'] == 'i586-unknown-linux-musl'
        ]}
    return {
        'enabled': 'true',
        'tag': tag,
        'source': source,
        'validation': main,
        'full_matrix': str(full_matrix).lower(),
        'rust_changed': str(rust_changed).lower(),
        'ish_target': str(ish_target).lower(),
        'repair_ish_only': str(repair_ish_only).lower(),
        'windows_companion': str(windows_companion).lower(),
        'windows_arm64': str(windows_arm64).lower(),
        'required_assets': str(required_assets),
        'rust_matrix': json.dumps(rust_matrix, separators=(',', ':')),
    }


if __name__ == '__main__':
    outputs = prepare()
    with open(os.environ['GITHUB_OUTPUT'], 'a') as handle:
        for key, value in outputs.items():
            handle.write(f'{key}={value}\n')
