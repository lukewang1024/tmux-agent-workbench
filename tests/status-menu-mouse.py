#!/usr/bin/env python3
"""Check real menu mouse dismissal and shortcuts through a tmux client PTY."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid

repo = Path(__file__).resolve().parents[1]
core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='wb-menu-mouse-') as root:
    root = Path(root)
    socket = str(root / 'tmux.sock')
    env = dict(os.environ, TERM='xterm-256color', TMUX_AGENT_WORKBENCH_BIN=core,
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
    for key in ('TMUX', 'TMUX_PANE'):
        env.pop(key, None)
    for key in ('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME'):
        env[key] = str(root / key)
    shim = root / 'bin'
    shim.mkdir()
    ssh = shim / 'ssh-connect'
    ssh.write_text('#!/bin/sh\nprintf "%s\\n" dev-host test-host\n')
    ssh.chmod(0o755)
    # macOS system binaries carry restricted BSD flags; copy bytes only.
    shutil.copyfile(shutil.which('cat'), shim / 'codex')
    (shim / 'codex').chmod(0o755)
    env['PATH'] = str(shim) + ':' + str(repo / 'bin') + ':' + env['PATH']
    project = root / 'project'
    for name in ('grill-me', 'handoff'):
        skill = project / '.agents/skills' / name
        skill.mkdir(parents=True)
        (skill / 'SKILL.md').write_text(f'---\nname: {name}\ndescription: Menu fixture\n---\n')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 80, 0, 0))

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def drain(seconds=0.15):
        output = b''
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([master], [], [], max(0, end - time.monotonic()))
            if ready:
                output += os.read(master, 65536)
        return output

    def item_position(output, label="close"):
        # Track the native menu's cursor writes to click the rendered Close row.
        screen = {}
        x = y = 0
        tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|[^\x1b]', output.decode(errors='replace'))
        for token in tokens:
            if token.startswith('\x1b['):
                command = token[-1]
                values = token[2:-1]
                if any(char not in '0123456789;' for char in values):
                    continue
                numbers = [int(value or 0) for value in values.split(';')]
                amount = numbers[0] or 1
                if command in ('H', 'f'):
                    y = amount - 1
                    x = (numbers[1] or 1) - 1 if len(numbers) > 1 else 0
                elif command == 'G': x = amount - 1
                elif command == 'd': y = amount - 1
                elif command == 'C': x += amount
                elif command == 'D': x -= amount
                elif command == 'A': y -= amount
                elif command == 'B': y += amount
                elif command == 'J' and numbers[0] == 2: screen.clear()
            elif token == '\r': x = 0
            elif token == '\n': y += 1
            elif token.isprintable():
                screen[x, y] = token
                x += 1
        for row in range(30):
            text = ''.join(screen.get((col, row), ' ') for col in range(80))
            if label in text:
                return text.index(label) + 2, row + 1
        raise AssertionError(('Menu item position missing', label, screen))

    def wait_for_text(proc, needle=b'close'):
        output = b''
        for _ in range(40):
            output += drain(0.05)
            if needle in output:
                return output
            if proc.poll() is not None:
                raise AssertionError(('menu exited before rendering', proc.returncode, proc.stderr.read()))
        raise AssertionError(('menu did not render', output))

    def write_frame(proc, message):
        payload = json.dumps(message, separators=(',', ':')).encode()
        proc.stdin.write(len(payload).to_bytes(4, 'big') + payload)
        proc.stdin.flush()

    def read_frame(proc):
        header = proc.stdout.read(4)
        if len(header) != 4:
            raise AssertionError(('client protocol closed before reply',
                                  proc.poll(), proc.stderr.read().decode(errors='replace')))
        length = int.from_bytes(header, 'big')
        payload = proc.stdout.read(length)
        if len(payload) != length:
            raise AssertionError(('truncated client protocol reply', payload))
        return json.loads(payload)

    def register_termux():
        proc = subprocess.Popen([core, 'client', 'serve'], env=env,
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        write_frame(proc, {
            'type': 'hello', 'version': 2, 'device_id': str(uuid.uuid4()),
            'terminal_id': str(uuid.uuid4()), 'device_label': 'test-phone',
            'kind': 'termux', 'capabilities': [],
        })
        welcome = read_frame(proc)
        assert welcome['type'] == 'welcome', welcome
        attach_env = dict(env, SSH_TTY=os.ttyname(slave))
        try:
            subprocess.run([core, 'client', 'attach-pty', '--bind', welcome['attachment_token']],
                           env=attach_env, stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=3)
        except subprocess.TimeoutExpired:
            pass
        return proc

    client_process = None
    menu = None
    termux = None
    daemon_started = False
    try:
        tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'audit', '-x', '80', '-y', '30', '-c', str(project), '/bin/sh')
        tmux('set-option', '-g', 'default-shell', '/bin/sh')
        tmux('set-option', '-g', 'mouse', 'on')
        tmux('set-option', '-g', 'status', 'off')
        tmux('set-option', '-g', '@workbench-usage-source', 'opencode')
        server_pid = tmux('display-message', '-p', '#{pid}')
        env['TMUX'] = f'{socket},{server_pid},0'
        env['TMUX_AGENT_WORKBENCH_TMUX_SOCKET'] = socket
        pane = tmux('display-message', '-p', '#{pane_id}')
        client_process = subprocess.Popen(['tmux', '-S', socket, 'attach-session', '-t', 'audit'],
                                          stdin=slave, stdout=slave, stderr=slave, env=env)
        for _ in range(40):
            client = tmux('list-clients', '-F', '#{client_name}')
            if client:
                break
            drain(0.05)
        drain()
        for kind in ('host', 'tmux', 'agent', 'usage', 'metrics'):
            for close in ('outside', 'escape', 'button'):
                args = ([str(repo / 'bin/workbench-agent-usage'), 'menu', client] if kind == 'usage'
                        else [str(repo / 'bin/workbench-host-metrics-menu'), client, pane] if kind == 'metrics'
                        else [str(repo / 'bin/workbench-status-popup'), kind, client, pane])
                menu = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                rendered = wait_for_text(menu)
                if close == 'outside':
                    os.write(master, b'\x1b[<0;1;1M\x1b[<0;1;1m')
                elif close == 'escape':
                    os.write(master, b'\x1b')
                else:
                    x, y = item_position(rendered)
                    os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
                for _ in range(40):
                    drain(0.05)
                    if menu.poll() is not None:
                        break
                assert menu.poll() == 0, (kind, close, menu.poll())
                assert tmux('list-windows', '-F', '#{window_id}').count('\n') == 0
                print('PASS', kind, close)
        # Exercise the status binding while this PTY represents a desktop
        # pointer. Termux's release-based selection is tested below.
        # Exercise the real status binding block: opening press/release must not
        # activate the first row, then mouse motion and click select one action.
        import shlex
        source = (repo / 'workbench.tmux').read_text()
        bindings = source[source.index('tmux bind-key -T root MouseDown1Status if-shell'):]
        bindings = bindings[:bindings.index('tmux bind-key -T root MouseDown3Status')]
        action = shlex.join([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane])
        route = ('wb-status-route=if-shell -F '
                 + shlex.quote('#{==:#{mouse_status_range},wb_tmux}') + ' '
                 + shlex.quote('run-shell ' + shlex.quote(action)) + ' '
                 + shlex.quote('select-window -t ='))
        tmux('set-option', '-s', 'command-alias[927]', route)
        subprocess.run(['sh', '-c', bindings], env=env, check=True)
        tmux('set-option', '-g', 'status', 'on')
        tmux('set-option', '-g', 'status-left', '')
        tmux('set-option', '-g', 'status-right', '#[range=user|wb_tmux]MENU#[range=]')
        drain()
        before = tmux('list-windows', '-F', '#{window_id}').splitlines()
        os.write(master, b'\x1b[<0;78;30M\x1b[<0;78;30m')
        rendered = wait_for_text(client_process)
        assert tmux('list-windows', '-F', '#{window_id}').splitlines() == before
        assert tmux('display-message', '-p', '-t', pane, '#{pane_in_mode}') == '0', 'opening status menu left an output pager'
        # The binding launches the status popup asynchronously; wait for its
        # mouse mode and rendering to settle before moving into the menu.
        drain(0.75)
        x, y = item_position(rendered, 'New window')
        os.write(master, f'\x1b[<35;{x};{y}M'.encode())
        drain(0.1)
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        after = before
        for _ in range(40):
            drain(0.05)
            after = tmux('list-windows', '-F', '#{window_id}').splitlines()
            if len(after) > len(before):
                break
        assert len(after) == len(before) + 1, (
            'status touch did not create exactly one window', before, after,
            tmux('capture-pane', '-p', '-t', pane),
            tmux('list-clients', '-F', '#{client_name} #{client_tty}'),
        )
        tmux('kill-window', '-t', next(window for window in after if window not in before))
        assert tmux('display-message', '-p', '-t', pane, '#{pane_in_mode}') == '0'
        # A normal window-tab click also must not leave an output pager behind.
        other = tmux('new-window', '-d', '-P', '-F', '#{window_id}', '-n', 'touch-target')
        tmux('set-option', '-g', 'window-status-format', '#W')
        tmux('set-option', '-g', 'window-status-current-format', '#W')
        tmux('refresh-client', '-S')
        rendered = drain(0.3)
        x, y = item_position(rendered, 'touch-target')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        drain(0.3)
        assert tmux('display-message', '-p', '#{window_id}') == other
        assert set(tmux('list-panes', '-s', '-F', '#{pane_in_mode}').splitlines()) == {'0'}
        tmux('kill-window', '-t', other)
        tmux('set-option', '-g', 'status', 'off')
        drain(0.3)
        print('PASS window tab touch leaves no output pager')
        print('PASS status opening release and subsequent mouse action')
        # Register this PTY as a Termux client so following checks use the
        # same no-hover menu path as a real phone attachment.
        subprocess.run([core, 'daemon', 'ensure'], env=env, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        daemon_started = True
        termux = register_termux()
        mouse_mode = 'mouse'
        for _ in range(40):
            mouse_mode = subprocess.check_output(
                [core, 'menu-mouse-mode', '--client', client], env=env, text=True
            ).strip()
            if mouse_mode == 'touch':
                break
            time.sleep(0.05)
        assert mouse_mode == 'touch', (
            'Termux attachment was not recognized for the tmux client',
            mouse_mode,
            tmux('display-message', '-p', '-c', client, '#{client_tty}'),
        )
        # A real foreground process identity is required: a shell must not
        # receive slash commands. A copied cat binary gives this fixture an
        # executable named codex without calling any model API.
        tmux('respawn-pane', '-k', '-t', pane, str(shim / 'codex'))
        tmux('select-pane', '-t', pane, '-T', 'Codex idle')
        drain(0.3)
        # Touch sends a press/release without any preceding hover/motion.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, '/side')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        drain(0.3)
        assert '/side' in tmux('capture-pane', '-p', '-t', pane), 'touch did not execute selected action'
        tmux('send-keys', '-t', pane, 'C-u')
        print('PASS touch selects action without hover')
        menu = subprocess.Popen([str(repo / 'bin/workbench-host-metrics-menu'), client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, 'Standard')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        for _ in range(40):
            if tmux('show-option', '-gqv', '@workbench-host-metrics-mode') == 'standard':
                break
            drain(0.05)
        assert tmux('show-option', '-gqv', '@workbench-host-metrics-mode') == 'standard'
        print('PASS metrics touch action')
        # The source stays fixed even if the client's active window changes.
        other = tmux('new-window', '-P', '-F', '#{window_id}', '-n', 'unrelated-shell', '/bin/sh')
        drain()
        # A native Agent action still prefills the source pane without Enter.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'/btw' not in rendered
        assert b'Focus this pane' not in rendered and b'Refresh' not in rendered
        assert b'/side' in rendered, (rendered, tmux('capture-pane', '-p', '-t', pane))
        os.write(master, b's')
        menu.wait(timeout=3)
        drain(0.3)
        assert '/side' in tmux('capture-pane', '-p', '-t', pane), tmux('capture-pane', '-p', '-t', pane)
        assert '/side' not in tmux('capture-pane', '-p', '-t', other)
        tmux('kill-window', '-t', other)
        drain()
        print('PASS agent action targets source pane after active window changes')
        for key, expected in [(b'q', '$grill-me'), (b'h', '$handoff')]:
            tmux('send-keys', '-t', pane, 'C-u')
            drain()
            menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            rendered = wait_for_text(menu)
            assert b'/goal' in rendered and b'grill-me' in rendered and b'handoff' in rendered
            assert b'/fork' not in rendered and b'/status' not in rendered
            os.write(master, key)
            menu.wait(timeout=3)
            drain(0.3)
            assert expected in tmux('capture-pane', '-p', '-t', pane)
        tmux('send-keys', '-t', pane, 'C-u')
        print('PASS goal and installed pinned skills on main menu; skills prefill only')
        tmux('select-pane', '-t', pane, '-T', '⠋ Codex')
        for _ in range(60):
            snapshot = json.loads(subprocess.check_output([core, 'snapshot', '--json'], env=env))
            if any(a['target']['pane_id'] == pane and a['base_state'] == 'working' for a in snapshot['agents']): break
            drain(0.05)
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'working' in rendered and b'grill-me' in rendered and b'handoff' in rendered
        assert b'/side' in rendered and b'/btw' not in rendered
        os.write(master, b'q')
        menu.wait(timeout=3)
        drain(0.3)
        assert '$grill-me' in tmux('capture-pane', '-p', '-t', pane)
        tmux('send-keys', '-t', pane, 'C-u')
        tmux('select-pane', '-t', pane, '-T', 'Codex idle')
        print('PASS working Codex retains skill shortcuts and uses /side')
        menu = subprocess.Popen([str(repo / 'bin/workbench-agent-usage'), 'menu', client],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        x, y = item_position(rendered, 'Claude Code')
        os.write(master, f'\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m'.encode())
        menu.wait(timeout=3)
        drain(0.5)
        assert tmux('show-option', '-gqv', '@workbench-usage-source') == 'claude'
        # The provider switch reopens Usage; wait for it to settle, then dismiss.
        os.write(master, b'\x1b')
        drain(0.6)
        print('PASS usage touch action')
        # Stale callbacks must not type into a replacement shell.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        tmux('respawn-pane', '-k', '-t', pane, '/bin/sh')
        os.write(master, b's')
        menu.wait(timeout=3)
        drain(0.4)
        assert '/side' not in tmux('capture-pane', '-p', '-t', pane)
        menu = subprocess.Popen([str(repo / 'bin/workbench-menu'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'/side' not in rendered and b'No foreground agent' in rendered
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        drain()
        missing = subprocess.run([core, 'status-menu', 'agent', '--pane', '%999999', '--client', client,
                                  '--guard', 'stale', '--action', '/side'], env=env, capture_output=True)
        assert missing.returncode == 0, missing.stderr
        assert tmux('display-message', '-p', '-t', pane, '#{pane_in_mode}') == '0'
        print('PASS stale/missing targets rejected without an output pager')

        # Follow the complete Single launch path through real submenus.
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        for key, expected in [(b'a', b'choose agent'), (b'x', b'choose preset'), (b's', b'Permissions:')]:
            os.write(master, key)
            wait_for_text(client_process, expected)
            drain(0.15)
        before = tmux('list-windows', '-F', '#{window_id}').splitlines()
        os.write(master, b's')
        for _ in range(40):
            drain(0.05)
            after = tmux('list-windows', '-F', '#{window_id}').splitlines()
            if len(after) > len(before): break
        assert len(after) == len(before) + 1
        launched = next(w for w in after if w not in before)
        assert tmux('display-message', '-p', '-t', launched, '#{pane_current_command}') == 'codex'
        assert tmux('display-message', '-p', '-t', launched, '#{pane_current_path}') == tmux('display-message', '-p', '-t', pane, '#{pane_current_path}')
        tmux('kill-window', '-t', launched)
        drain()
        print('PASS Single launch wizard preserves source cwd')
        config_dir = Path(env['XDG_CONFIG_HOME']) / 'tmux-agent-workbench'
        config_dir.mkdir(parents=True, exist_ok=True)
        # cat's -u is a harmless fixture argument proving that the configured
        # budget arguments are passed to a single interactive CLI, not a team.
        (config_dir / 'launch.toml').write_text("[single_budget]\ncodex = ['-u']\n")
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        for key, heading in [(b'a', b'choose agent'), (b'x', b'Single Budget'), (b'b', b'Permissions:')]:
            os.write(master, key)
            wait_for_text(client_process, heading)
            drain(0.15)
        before = tmux('list-windows', '-F', '#{window_id}').splitlines()
        os.write(master, b's')
        for _ in range(40):
            drain(0.05)
            after = tmux('list-windows', '-F', '#{window_id}').splitlines()
            if len(after) > len(before): break
        assert len(after) == len(before) + 1
        budget_window = next(w for w in after if w not in before)
        pid = tmux('display-message', '-p', '-t', budget_window, '#{pane_pid}')
        argv = subprocess.check_output(['ps', '-o', 'args=', '-p', pid], text=True)
        assert 'codex -u' in argv, argv
        tmux('kill-window', '-t', budget_window)
        (config_dir / 'launch.toml').unlink()
        drain()
        print('PASS Single Budget uses configured arguments and exactly one window')

        # Record launches at the public agent-team boundary, without paid API
        # calls. The pane backend creates its own window; no launcher survives.
        trace = root / 'team-launch.txt'
        team_cli = shim / 'agent-team'
        team_cli.write_text('#!/bin/sh\nset -eu\n' +
            'printf "%s\\n" "$PWD" "$@" > ' + shlex.quote(str(trace)) + '\n' +
            'case " $* " in *" --tmux "*)\n' +
            'session=$(tmux display-message -p -t "$TMUX_PANE" "#{session_id}")\n' +
            'tmux new-window -t "$session" -n test-team -c "$PWD" ' + shlex.quote(str(shim / 'codex')) + '\n' +
            ';; *) exec ' + shlex.quote(str(shim / 'codex')) + ' ;; esac\n')
        team_cli.chmod(0o755)
        for preset, presentation, expected in [(b't', b'n', ['codex']), (b'T', b't', ['codex', '--team-budget', '--tmux'])]:
            menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            wait_for_text(menu)
            for key, heading in [(b'a', b'choose agent'), (b'x', b'choose preset'), (preset, b'Native subagents'), (presentation, b'Permissions:')]:
                os.write(master, key)
                wait_for_text(client_process, heading)
                drain(0.15)
            before = tmux('list-windows', '-F', '#{window_id}').splitlines()
            os.write(master, b's')
            for _ in range(60):
                drain(0.05)
                after = tmux('list-windows', '-F', '#{window_id}').splitlines()
                if len(after) > len(before) and trace.exists(): break
            assert len(after) == len(before) + 1, (before, after)
            recorded = trace.read_text().splitlines()
            source_cwd = tmux('display-message', '-p', '-t', pane, '#{pane_current_path}')
            assert Path(recorded[0]).samefile(source_cwd), (recorded[0], source_cwd)
            assert recorded[1:] == expected, recorded
            tmux('kill-window', '-t', next(w for w in after if w not in before))
            trace.unlink()
            drain()
        print('PASS native Team and tmux Team Budget launch exactly one window')

        tmux('respawn-pane', '-k', '-t', pane, str(shim / 'codex'))
        tmux('select-pane', '-t', pane, '-T', 'Action Required')
        drain(0.2)
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'/side' not in rendered and b'Return to the agent prompt' in rendered
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        tmux('select-pane', '-t', pane, '-T', 'Codex idle')
        tmux('copy-mode', '-t', pane)
        drain()
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        assert b'/side' not in rendered
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        tmux('send-keys', '-t', pane, '-X', 'cancel')
        print('PASS approval and copy mode suppress agent input')

        window = tmux('display-message', '-p', '-t', pane, '#{window_id}')
        worker = tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-t', pane, str(shim / 'codex'))
        for member in (pane, worker): tmux('set-option', '-p', '-t', member, '@agent_team', 'menu-test')
        state_dir = Path(env['XDG_STATE_HOME']) / 'agent-team/menu-test'
        state_dir.mkdir(parents=True)
        (state_dir / 'state.json').write_text(json.dumps({'id': 'menu-test', 'window': window, 'socket': socket,
            'members': {'lead': {'pane': pane, 'status': 'running'}, 'worker_a': {'pane': worker, 'status': 'ready'}}}))
        drain()
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b't')
        rendered = wait_for_text(client_process, b'worker_a')
        assert b'lead' in rendered and b'ready' in rendered
        drain(0.2)
        # Team layout uses the same sidebar-preserving helper as Alt+4.
        sidebar = tmux('split-window', '-d', '-P', '-F', '#{pane_id}', '-h', '-b', '-l', '18', '-t', pane, '/bin/sh')
        tmux('set-option', '-p', '-t', sidebar, '@pane_role', 'sidebar')
        tmux('set-option', '-g', '@sidebar_width', '18')
        os.write(master, b'L')
        for _ in range(40):
            drain(0.05)
            if tmux('show-option', '-wqv', '-t', window, '@workbench_layout_preset') == 'main-vertical': break
        drain(0.3)
        assert tmux('display-message', '-p', '-t', sidebar, '#{pane_width}') == '18'
        tmux('kill-pane', '-t', sidebar)
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'agent', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b't')
        wait_for_text(client_process, b'worker_a')
        drain(0.2)
        os.write(master, b'2')
        for _ in range(40):
            drain(0.05)
            if tmux('display-message', '-p', '#{pane_id}') == worker: break
        assert tmux('display-message', '-p', '#{pane_id}') == worker
        tmux('kill-pane', '-t', worker)
        tmux('set-option', '-p', '-u', '-t', pane, '@agent_team')
        drain()
        print('PASS Team member roles, reported progress and pane navigation')

        # Narrow clients retain usable menus and Close rather than rejecting width.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 30, 0, 0))
        import signal
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        menu = subprocess.Popen([str(repo / 'bin/workbench-status-popup'), 'tmux', client, pane],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_text(menu)
        os.write(master, b'\x1b')
        menu.wait(timeout=3)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 10, 30, 0, 0))
        client_process.send_signal(signal.SIGWINCH)
        drain(0.3)
        menu = subprocess.Popen([core, 'status-menu', 'tmux', '--pane', pane, '--client', client,
                                 '--view', 'preset'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        rendered = wait_for_text(menu)
        _, back_y = item_position(rendered, '← Back')
        back_key_x, _ = item_position(rendered, '(B)')
        close_key_x, _ = item_position(rendered, '(esc)')
        assert back_key_x + len('(B)') == close_key_x + len('(esc)'), (back_key_x, close_key_x, rendered)
        _, close_y = item_position(rendered, 'close')
        assert close_y == back_y + 1, (back_y, close_y)
        os.write(master, b']')
        rendered = wait_for_text(client_process)
        _, back_y = item_position(rendered, '← Back')
        back_key_x, _ = item_position(rendered, '(B)')
        close_key_x, _ = item_position(rendered, '(esc)')
        assert back_key_x + len('(B)') == close_key_x + len('(esc)'), (back_key_x, close_key_x, rendered)
        _, close_y = item_position(rendered, 'close')
        assert close_y == back_y + 1
        os.write(master, b'B')
        wait_for_text(client_process, b'choose agen')
        os.write(master, b'\x1b')
        drain()
        print('PASS Back stays beside Close across submenu pages')
        print('PASS narrow client with paginated actions')
        write_frame(termux, {'type': 'goodbye', 'version': 2})
        termux.wait(timeout=3)
    finally:
        if menu is not None and menu.poll() is None:
            menu.terminate()
        subprocess.run(['tmux', '-S', socket, 'kill-server'], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if client_process is not None:
            client_process.wait(timeout=3)
        if termux is not None and termux.poll() is None:
            termux.terminate()
            termux.wait(timeout=3)
        if daemon_started:
            subprocess.run([core, 'daemon', 'stop'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(master)
        os.close(slave)
