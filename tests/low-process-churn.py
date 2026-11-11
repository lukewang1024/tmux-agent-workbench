#!/usr/bin/env python3
"""Measure the opt-in idle budget and event wakeup on an isolated macOS server."""
import json
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import socket as socket_module
import struct
import subprocess
import sys
import tempfile
import time
import uuid

if sys.platform != 'darwin':
    print('SKIP low-process-churn macOS process-event measurement')
    raise SystemExit(0)

import fcntl
import termios

core = str(Path(sys.argv[1]).resolve())
fixture = str(Path(core).parent / 'examples/codex')
tmux_binary = shutil.which('tmux')
if not tmux_binary or not Path(fixture).exists():
    raise SystemExit('tmux and the codex example are required')

with tempfile.TemporaryDirectory(prefix='wb-idle-', dir='/tmp') as directory:
    root = Path(directory)
    env = dict(os.environ, TERM='xterm-256color', WORKBENCH_FIXTURE_SECONDS='180')
    for key in ('TMUX', 'TMUX_PANE', 'TMUX_TMPDIR', 'WORKBENCH_POPUP'):
        env.pop(key, None)
    for key in ('XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        env[key] = str(root / key)
    # Debug builds record command construction without starting a wrapper
    # process; kqueue notifications alone can coalesce multiple fast forks.
    trace = root / 'tmux-invocations.log'
    trace.touch(mode=0o600)
    env['WORKBENCH_TEST_SPAWN_TRACE'] = str(trace)
    probe_errors = root / 'probe-errors.log'
    env['WORKBENCH_TEST_PROBE_ERRORS'] = str(probe_errors)
    # If session rendering regresses to launching Git, record it too. This
    # wrapper is never executed by the intended process-free implementation.
    real_git = shutil.which('git')
    if not real_git:
        raise SystemExit('git is required for the session-view regression')
    shim_dir = root / 'bin'
    shim_dir.mkdir()
    shim = shim_dir / 'git'
    shim.write_text('#!/bin/sh\nprintf "%s\\tgit\\n" "$PPID" >> "$WORKBENCH_TEST_SPAWN_TRACE"\n'
                    + 'exec ' + shlex.quote(real_git) + ' "$@"\n')
    shim.chmod(0o700)
    # Record actual tmux queries separately from process-construction counts.
    # The shell immediately execs the real tmux, without an extra child.
    tmux_trace = root / 'tmux-queries.log'
    tmux_trace.touch(mode=0o600)
    env['WORKBENCH_TEST_TMUX_TRACE'] = str(tmux_trace)
    stall_probe = root / 'stall-surface-probe'
    env['WORKBENCH_TEST_TMUX_STALL'] = str(stall_probe)
    fail_probe = root / 'fail-surface-probe'
    env['WORKBENCH_TEST_TMUX_FAIL'] = str(fail_probe)
    tmux_shim = shim_dir / 'tmux'
    tmux_shim.write_text('#!/bin/sh\nprintf "%s\\t%s\\n" "$PPID" "$*" >> "$WORKBENCH_TEST_TMUX_TRACE"\n'
                         + 'case "$*" in *"@workbench_team_restore_token"*) '
                         + '[ ! -f "$WORKBENCH_TEST_TMUX_STALL" ] || /bin/sleep 2; '
                         + '[ ! -f "$WORKBENCH_TEST_TMUX_FAIL" ] || exit 1 ;; esac\n'
                         + 'exec ' + shlex.quote(tmux_binary) + ' "$@"\n')
    tmux_shim.chmod(0o700)
    env['PATH'] = str(shim_dir) + os.pathsep + env.get('PATH', os.defpath)
    socket = str(root / 'tmux.sock')
    env['TMUX_AGENT_WORKBENCH_TMUX_SOCKET'] = socket
    config = Path(env['XDG_CONFIG_HOME']) / 'tmux-agent-workbench/config.toml'
    config.parent.mkdir(parents=True)
    config.write_text('[ui]\nlanguage = "en-US"\n')
    (config.parent / 'enable-low-process-churn').touch()
    view = Path(env['XDG_STATE_HOME']) / 'tmux-agent-workbench/sidebar-view'
    view.parent.mkdir(parents=True)
    view.write_text('sessions\n')
    workspace = root / 'repo'
    (workspace / '.git').mkdir(parents=True)
    head = workspace / '.git/HEAD'
    head.write_text('ref: refs/heads/branch-one\n')
    tmux_args = [tmux_binary, '-S', socket]
    daemon = None
    client = None
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))

    def tmux(*args):
        return subprocess.check_output(tmux_args + list(args), env=env, text=True).strip()

    def ipc_call(method, params=None):
        path, = (Path(env['XDG_RUNTIME_DIR']) / 'tmux-agent-workbench').glob('daemon-*.sock')
        request = {'protocol_version': 1, 'id': 'surface-test', 'method': method,
                   'params': params or {}}
        with socket_module.socket(socket_module.AF_UNIX) as conn:
            conn.settimeout(3)
            conn.connect(str(path))
            conn.sendall(json.dumps(request).encode() + b'\n')
            response = json.loads(conn.makefile('rb').readline())
        assert response['ok'], response
        return response['result']

    def ipc_snapshot(include_surface=False, refresh_surface=False):
        return ipc_call('snapshot.get', {'include_surface': include_surface,
                                         'refresh_surface': refresh_surface})

    def snapshot():
        return ipc_snapshot()

    def surface():
        return ipc_snapshot(True).get('_workbench_surface')

    def wait_for(predicate, description, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(.1)
        raise AssertionError(description)

    try:
        idle = "printf '\\n› Ask Codex to do anything\\n  GPT-6-Sol high · ~\\n'; exec " + shlex.quote(fixture)
        panes = [tmux('-f', '/dev/null', 'new-session', '-d', '-P', '-F', '#{pane_id}',
                      '-s', 'test', '-x', '160', '-y', '40', '-c', str(workspace), idle)]
        panes.append(tmux('new-window', '-d', '-P', '-F', '#{pane_id}', '-t', 'test:', '-c', str(workspace), idle))
        panes.append(tmux('new-session', '-d', '-P', '-F', '#{pane_id}', '-s', 'background', '-c', str(workspace), idle))
        panes.append(tmux('new-window', '-d', '-P', '-F', '#{pane_id}', '-t', 'test:', '-c', str(workspace), 'sleep 180'))
        tmux('set-option', '-g', 'focus-events', 'on')
        tmux('set-hook', '-g', 'client-attached[921]', 'display-message preexisting-hook')
        daemon = subprocess.Popen([core, 'daemon', 'run'], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_for(lambda: subprocess.run([core, 'daemon', 'status'], env=env,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
                 'daemon did not start')
        sidebars = []
        for pane in panes:
            sidebar = tmux('split-window', '-d', '-h', '-l', '36', '-t', pane,
                           '-P', '-F', '#{pane_id}', core, 'sidebar')
            tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
            sidebars.append(sidebar)
        client = subprocess.Popen(tmux_args + ['attach-session', '-t', 'test'], env=env,
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        wait_for(lambda: tmux('list-clients', '-F', '#{client_name}'),
                 'tmux client did not attach')
        client_name = tmux('list-clients', '-F', '#{client_name}')
        os.write(master, b'\x1b[O')
        wait_for(lambda: 'focused' not in tmux('list-clients', '-F', '#{client_flags}'),
                 'fixture client did not enter the unfocused state')
        assert 'preexisting-hook' in tmux('show-hooks', '-g', 'client-attached')
        assert 'workbench-low-churn-focus-wake' in tmux('show-hooks', '-g', 'client-session-changed')

        def idle_agents():
            agents = [a for a in snapshot()['agents'] if not a['exited']]
            return agents if len(agents) == 3 and all(a['base_state'] == 'idle' and not a['stale'] for a in agents) else None

        agents = wait_for(idle_agents, 'fixture agents never settled idle')
        # The native macOS foreground check must preserve the interactive flag.
        wait_for(lambda: all(tmux('show-option', '-pqv', '-t', pane,
                                  '@workbench_agent_actions') == '1' for pane in panes[:3]),
                 'foreground agents were not marked interactive')
        # The fixture does not report terminal focus. An attached session must
        # still be sampled promptly without changing its attention semantics.
        wait_for(lambda: (surface() or {}).get('visibility', {}).get(sidebars[0]) is True,
                 'attached session was not recognized')
        wait_for(lambda: 'branch-one' in tmux('capture-pane', '-p', '-t', sidebars[0]),
                 'session view did not display the current branch')
        started = time.monotonic()
        head.write_text('ref: refs/heads/branch-two\n')
        wait_for(lambda: 'branch-two' in tmux('capture-pane', '-p', '-t', sidebars[0]),
                 'session view did not update after a branch change', timeout=8)
        print(f'PASS foreground branch update in {time.monotonic() - started:.2f}s', flush=True)

        def state_for(pane):
            return next((a['base_state'] for a in snapshot()['agents']
                         if a['target']['pane_id'] == pane and not a['exited']), None)

        # A second window in the foreground session must not inherit the
        # background sampling delay, even when that window is not selected.
        started = time.monotonic()
        tmux('select-pane', '-t', panes[1], '-T', '⠴ foreground test')
        wait_for(lambda: state_for(panes[1]) == 'working', 'foreground window update was delayed', timeout=8)
        print(f'PASS foreground session screen update in {time.monotonic() - started:.2f}s', flush=True)
        tmux('select-pane', '-t', panes[1], '-T', 'Ready')
        wait_for(idle_agents, 'foreground agent did not return to idle')

        # The shared palette and inventory should follow external tmux changes
        # without asking each sidebar to launch its own tmux process.
        tmux('set-option', '-g', '@adaptive_appearance', 'light')
        wait_for(lambda: (surface() or {}).get('appearance') == 'light',
                 'foreground theme did not refresh', timeout=8)
        background_pane = panes[2]
        tmux('select-pane', '-t', background_pane, '-T', '⠴ background test')
        # Foreground detection must also work if a tmux hook is delayed or
        # absent. Temporarily remove just our isolated fixture's wake hook.
        hook_line = next(line for line in tmux('show-hooks', '-g', 'client-session-changed').splitlines()
                         if line.startswith('client-session-changed[921] '))
        hook_command = hook_line.split(' ', 1)[1]
        tmux('set-hook', '-gu', 'client-session-changed[921]')
        try:
            started = time.monotonic()
            tmux('switch-client', '-c', client_name, '-t', 'background')
            wait_for(lambda: state_for(background_pane) == 'working',
                     'switching to a background session did not wake it', timeout=8)
        finally:
            tmux('set-hook', '-g', 'client-session-changed[921]', hook_command)
        print(f'PASS session switch wakes detection in {time.monotonic() - started:.2f}s', flush=True)
        tmux('select-pane', '-t', background_pane, '-T', 'Ready')
        tmux('switch-client', '-c', client_name, '-t', 'test')
        agents = wait_for(idle_agents, 'agents did not settle after foreground switch')

        # A failed shared probe can make detached idle agents stale. Once tmux
        # recovers, they must be included in the next shared capture instead
        # of waiting for their much slower normal background deadline.
        fail_probe.touch()
        try:
            def all_captures_stale():
                sample = ipc_snapshot(True, True)
                live = [a for a in sample['agents'] if not a['exited']]
                return len(live) == 3 and all(a['stale'] for a in live)

            wait_for(all_captures_stale, 'failed idle probe did not mark captures stale', timeout=12)
        finally:
            fail_probe.unlink()
        started = time.monotonic()
        try:
            agents = wait_for(idle_agents, 'detached stale capture did not recover promptly', timeout=8)
        except AssertionError as error:
            live = [(a['target']['pane_id'], a['target']['session_name'], a['base_state'], a['stale'])
                    for a in snapshot()['agents'] if not a['exited']]
            errors = probe_errors.read_text() if probe_errors.exists() else 'none'
            raise AssertionError(f'{error}; agents={live}; probe_errors={errors}') from error
        print(f'PASS detached stale capture recovered in {time.monotonic() - started:.2f}s', flush=True)

        sidebar_pids = [int(tmux('display-message', '-p', '-t', pane, '#{pane_pid}')) for pane in sidebars]
        server_pid = int(tmux('display-message', '-p', '#{pid}'))
        # Also watch tmux itself: Workbench wake hooks start there, rather than
        # as children of the daemon or a sidebar.
        pids = [daemon.pid] + sidebar_pids + [server_pid]
        time.sleep(5)  # Ignore startup and the transition into idle scheduling.
        queue = select.kqueue()
        for pid in pids:
            queue.control([select.kevent(pid, filter=select.KQ_FILTER_PROC,
                                         flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                                         fflags=select.KQ_NOTE_FORK)], 0, 0)
        counts = {pid: 0 for pid in pids}
        start = time.monotonic()
        next_sample = start
        nonidle_samples = []
        stale_samples = []
        previously_stale = set()
        persistent_stale = set()
        with trace.open('a') as stream:
            stream.write('MEASURE_START\n')
        while time.monotonic() - start < 60:
            for event in queue.control(None, 128, .03):
                if event.fflags & select.KQ_NOTE_FORK:
                    counts[event.ident] += 1
            if select.select([master], [], [], 0)[0]:
                os.read(master, 65536)
            if time.monotonic() >= next_sample:
                sample = ipc_snapshot(True)
                live = [a for a in sample['agents'] if not a['exited']]
                if len(live) != 3 or any(a['base_state'] != 'idle' for a in live):
                    nonidle_samples.append([(a['base_state'], a['stale']) for a in live])
                stale = {a['instance_id'] for a in live if a['stale']}
                if stale:
                    stale_samples.append((round(time.monotonic() - start, 2),
                                          [(a['target']['pane_id'], a['target']['session_name'])
                                           for a in live if a['stale']]))
                persistent_stale.update(stale & previously_stale)
                previously_stale = stale
                next_sample += 5
        elapsed = time.monotonic() - start
        with trace.open('a') as stream:
            stream.write('MEASURE_END\n')
        queue.close()
        assert daemon.poll() is None and client.poll() is None, 'resident process exited during measurement'
        for pane, pid in zip(sidebars, sidebar_pids):
            state = tmux('display-message', '-p', '-t', pane, '#{pane_pid}:#{pane_dead}')
            assert state == f'{pid}:0', f'sidebar changed during measurement: {pane} {state}'
        events = sum(counts.values())
        lines = trace.read_text().splitlines()
        measured = lines[lines.index('MEASURE_START') + 1:lines.index('MEASURE_END')]
        invocation_breakdown = Counter()
        for line in measured:
            parent, _, command = line.partition('\t')
            if parent.isdigit() and int(parent) in counts:
                invocation_breakdown[(int(parent), command)] += 1
        invocations = sum(invocation_breakdown.values())
        print(f'Idle command construction breakdown: {dict(invocation_breakdown)}', flush=True)
        assert not nonidle_samples, f'agents did not stay idle during measurement: {nonidle_samples}'
        assert not persistent_stale, (
            f'capture health did not recover promptly: {persistent_stale}; '
            f'samples={stale_samples}; probe_errors={probe_errors.read_text() if probe_errors.exists() else "none"}')
        if previously_stale:
            wait_for(idle_agents, 'capture health did not recover after measurement', timeout=5)
        assert invocations > 0, 'debug spawn accounting did not record the resident processes'
        assert invocations <= elapsed, f'idle command budget exceeded: {invocations} in {elapsed:.1f}s'
        assert events <= elapsed, f'idle fork budget exceeded: {events} in {elapsed:.1f}s, {counts}'
        print(f'PASS idle budget: {invocations} command constructions, {events} fork notifications '
              f'in {elapsed:.1f}s, daemon, four sidebars and tmux server {counts}')

        # A lifecycle event must wake idle detection without waiting for its timer.
        agent = next(a for a in agents if a['target']['pane_id'] == panes[0])
        session = str(uuid.uuid4())
        started = time.monotonic()
        subprocess.run([core, 'hook', 'ingest', 'codex', 'UserPromptSubmit'], env=env,
                       input=json.dumps({'session_id': session, 'agent_pid': agent['process']['pid']}),
                       text=True, capture_output=True, check=True, timeout=5)
        wait_for(lambda: any(a['instance_id'] == agent['instance_id'] and a['base_state'] == 'working'
                             for a in snapshot()['agents']), 'hook did not wake the idle agent', timeout=3)
        delay = time.monotonic() - started
        assert delay < 3, f'hook wakeup took {delay:.2f}s'
        print(f'PASS lifecycle event wakes idle agent in {delay:.2f}s')

        # Four UIs and many simultaneous refresh requests must share one
        # rate-limited core query even while an agent is working. The visible
        # branch still changes promptly, and no sidebar starts tmux itself.
        wait_for(lambda: (surface() or {}).get('sidebar'),
                 'active shared sidebar data was not published', timeout=8)
        started = time.monotonic()
        head.write_text('ref: refs/heads/branch-active\n')
        wait_for(lambda: 'branch-active' in tmux('capture-pane', '-p', '-t', sidebars[0]),
                 'active foreground branch update was delayed', timeout=8)
        print(f'PASS active foreground branch update in {time.monotonic() - started:.2f}s')
        time.sleep(3)
        with trace.open('a') as stream:
            stream.write('ACTIVE_START\n')
        with tmux_trace.open('a') as stream:
            stream.write('ACTIVE_START\n')
        start = time.monotonic()
        with ThreadPoolExecutor(max_workers=4) as pool:
            while time.monotonic() - start < 6:
                list(pool.map(lambda _: ipc_snapshot(True, True), range(4)))
                time.sleep(.15)
        elapsed = time.monotonic() - start
        with trace.open('a') as stream:
            stream.write('ACTIVE_END\n')
        with tmux_trace.open('a') as stream:
            stream.write('ACTIVE_END\n')
        lines = trace.read_text().splitlines()
        active = lines[lines.index('ACTIVE_START') + 1:lines.index('ACTIVE_END')]
        sidebar_calls = [line for line in active if line.partition('\t')[0].isdigit()
                         and int(line.partition('\t')[0]) in sidebar_pids]
        queries = tmux_trace.read_text().splitlines()
        queries = queries[queries.index('ACTIVE_START') + 1:queries.index('ACTIVE_END')]
        surface_queries = [line for line in queries if line.startswith(str(daemon.pid) + '\t')
                           and '@workbench_team_restore_token' in line]
        assert not sidebar_calls, f'sidebars started background commands: {sidebar_calls}'
        assert surface_queries, 'core did not refresh its shared inventory'
        assert all(line.count('list-panes') == 1 for line in surface_queries), (
            f'a shared refresh repeated the real pane inventory: {surface_queries}')
        assert len(surface_queries) <= int(elapsed) + 2, (
            f'core inventory queries were amplified: {len(surface_queries)} in {elapsed:.1f}s')
        print(f'PASS four active sidebars used core only; {len(surface_queries)} shared queries in {elapsed:.1f}s')

        # Even if a real tmux inventory stalls, all UIs must be able to read
        # the last cached frame without waiting for the detector's state lock.
        offset = tmux_trace.stat().st_size
        stall_probe.touch()
        try:
            wait_for(lambda: '@workbench_team_restore_token' in tmux_trace.read_text()[offset:],
                     'the deliberately stalled surface query did not start', timeout=8)
            started = time.monotonic()
            with ThreadPoolExecutor(max_workers=4) as pool:
                list(pool.map(lambda _: ipc_snapshot(True, True), range(4)))
            elapsed = time.monotonic() - started
            assert elapsed < 1.5, f'four cached IPC reads waited {elapsed:.2f}s for a stalled query'
            print(f'PASS four cached IPC reads returned in {elapsed:.2f}s during a stalled query')
        finally:
            stall_probe.unlink()
        wait_for(surface, 'shared cache did not recover after the stalled query', timeout=8)

        # A failing inventory is also rate limited: the four sidebars must
        # not turn one failed query into independent retries or fallbacks.
        with tmux_trace.open('a') as stream:
            stream.write('FAIL_START\n')
        fail_probe.touch()
        started = time.monotonic()
        try:
            with ThreadPoolExecutor(max_workers=4) as pool:
                while time.monotonic() - started < 3:
                    list(pool.map(lambda _: ipc_snapshot(True, True), range(4)))
                    time.sleep(.15)
        finally:
            fail_probe.unlink()
        elapsed = time.monotonic() - started
        with tmux_trace.open('a') as stream:
            stream.write('FAIL_END\n')
        lines = tmux_trace.read_text().splitlines()
        failed = lines[lines.index('FAIL_START') + 1:lines.index('FAIL_END')]
        inventories = [line for line in failed if line.startswith(str(daemon.pid) + '\t')
                       and ' list-panes ' in line]
        assert inventories, 'the failing shared inventory was not exercised'
        assert len(inventories) <= int(elapsed) + 2, (
            f'failed inventories caused unthrottled retries: {len(inventories)} in {elapsed:.1f}s')
        print(f'PASS failed shared inventory stayed rate-limited: {len(inventories)} queries in {elapsed:.1f}s')
        wait_for(surface, 'shared cache did not recover after inventory failures', timeout=8)

        marker = config.parent / 'enable-low-process-churn'
        marker.unlink()
        ipc_call('config.reload')
        assert 'workbench-low-churn-focus-wake' not in tmux('show-hooks', '-g', 'client-session-changed')
        assert 'preexisting-hook' in tmux('show-hooks', '-g', 'client-attached')
        marker.touch()
        ipc_call('config.reload')
        assert 'workbench-low-churn-focus-wake' in tmux('show-hooks', '-g', 'client-session-changed')
        print('PASS focus hooks preserve existing hooks and follow configuration reload')
        subprocess.run([core, 'daemon', 'stop'], env=env, check=True, capture_output=True, timeout=5)
        daemon.wait(timeout=10)
        with trace.open('a') as stream:
            stream.write('STOPPED_START\n')
        time.sleep(3)
        with trace.open('a') as stream:
            stream.write('STOPPED_END\n')
        lines = trace.read_text().splitlines()
        stopped = lines[lines.index('STOPPED_START') + 1:lines.index('STOPPED_END')]
        assert not [line for line in stopped if line.partition('\t')[0].isdigit()
                    and int(line.partition('\t')[0]) in sidebar_pids], 'sidebars queried tmux while core was stopped'
        subprocess.run([core, 'daemon', 'wake-if-running'], env=env, check=True, capture_output=True, timeout=5)
        assert subprocess.run([core, 'daemon', 'status'], env=env, capture_output=True, timeout=5).returncode != 0
        print('PASS a late focus wake does not restart a stopped daemon')
    finally:
        try:
            # The server is an isolated test fixture. Remove its wake hooks
            # before detaching the client, so cleanup cannot start new work.
            hooks = ('client-attached', 'client-detached', 'client-session-changed',
                     'session-window-changed', 'window-pane-changed', 'client-focus-in', 'client-focus-out')
            commands = []
            for hook in hooks:
                if commands:
                    commands.append(';')
                commands.extend(('set-hook', '-gu', f'{hook}[921]'))
            subprocess.run(tmux_args + commands, env=env, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=5)
            if client is not None and client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait(timeout=5)
        finally:
            try:
                if daemon is not None and daemon.poll() is None:
                    subprocess.run([core, 'daemon', 'stop'], env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, timeout=5)
                    try:
                        daemon.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=5)
            finally:
                try:
                    subprocess.run(tmux_args + ['kill-server'], env=env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, timeout=5)
                finally:
                    os.close(master)
                    os.close(slave)
