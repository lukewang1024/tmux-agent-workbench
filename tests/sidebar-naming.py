"""Exercise real tmux automatic renaming and responsive argument validation."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
repo = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='wb-naming-') as root:
    socket = root + '/tmux.sock'
    env = dict(os.environ, TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket,
               XDG_CONFIG_HOME=root+'/config', XDG_STATE_HOME=root+'/state',
               XDG_CACHE_HOME=root+'/cache', XDG_RUNTIME_DIR=root, TMUX=socket+',0,0')

    def tmux(*args):
        return subprocess.check_output(['tmux', '-S', socket, *args], env=env, text=True).strip()

    def core(*args):
        subprocess.run([binary, *args], env=env, check=True)

    def title(pane, value):
        tty = tmux('display-message', '-p', '-t', pane, '#{pane_tty}')
        with open(tty, 'w') as stream:
            stream.write('\x1b]2;' + value + '\x07')
            stream.flush()

    def wait_command(pane, expected):
        # On macOS a freshly forked pane can still report its parent's "tmux"
        # name before exec. Select it only once the intended process is ready.
        for _ in range(80):
            if tmux('display-message', '-p', '-t', pane, '#{pane_current_command}') == expected:
                return
            time.sleep(.05)
        raise AssertionError(('pane process did not start', pane, expected))

    def wait_name(expected):
        for _ in range(80):
            if tmux('display-message', '-p', '-t', window, '#{window_name}') == expected:
                return
            time.sleep(.05)
        raise AssertionError((expected, tmux('display-message', '-p', '-t', window, '#{window_name}')))

    try:
        main = tmux('-f', '/dev/null', 'new-session', '-d', '-x', '160', '-y', '40',
                    '-P', '-F', '#{pane_id}', 'sleep', '120')
        wait_command(main, 'sleep')
        window = tmux('display-message', '-p', '-t', main, '#{window_id}')
        # A queued resize hook may outlive a short-lived placeholder window.
        retired = tmux('new-window', '-d', '-P', '-F', '#{window_id}', 'sleep', '120')
        tmux('kill-window', '-t', retired)
        before = tmux('list-panes', '-a', '-F', '#{window_id}:#{pane_id}:#{pane_width}x#{pane_height}')
        stale = subprocess.run([binary, 'sidebar-control', 'maintain', retired],
                               env=env, capture_output=True, text=True)
        assert stale.returncode == 0 and not stale.stderr, (stale.returncode, stale.stderr)
        assert tmux('list-panes', '-a', '-F', '#{window_id}:#{pane_id}:#{pane_width}x#{pane_height}') == before
        malformed = subprocess.run([binary, 'sidebar-control', 'maintain', 'invalid'],
                                   env=env, capture_output=True, text=True)
        assert malformed.returncode != 0
        tmux('set-option', '-w', '-t', window, 'automatic-rename', 'on')
        core('sidebar-control', 'toggle', window)
        assert tmux('display-message', '-p', '-t', window, '#{pane_id}') == main
        assert tmux('show-option', '-wv', '-t', window, 'automatic-rename') == 'on'
        wait_name('sleep')
        sidebar = tmux('list-panes', '-t', window, '-f', '#{==:#{@pane_role},sidebar}', '-F', '#{pane_id}')
        tmux('select-pane', '-t', sidebar)
        time.sleep(1)
        wait_name('sleep')
        other = tmux('split-window', '-d', '-t', main, '-P', '-F', '#{pane_id}', 'tail', '-f', '/dev/null')
        wait_command(other, 'tail')
        tmux('select-pane', '-t', other)
        wait_name('tail')
        tmux('select-pane', '-t', main)
        wait_name('sleep')
        # Explicit names remain untouched, including when the sidebar is closed.
        tmux('rename-window', '-t', window, 'custom')
        tmux('select-pane', '-t', sidebar)
        time.sleep(1)
        wait_name('custom')
        core('sidebar-control', 'toggle', window)
        assert tmux('show-option', '-wv', '-t', window, 'automatic-rename') == 'off'
        # Legacy frozen windows regain their original automatic naming.
        tmux('set-option', '-w', '-t', window, '@workbench_saved_automatic_rename', 'on')
        core('sidebar-control', 'toggle', window)
        wait_name('sleep')
        # Preserve the user's custom automatic format across sidebar cycles.
        core('sidebar-control', 'toggle', window)
        tmux('set-option', '-w', '-t', window, 'automatic-rename-format', 'cmd:#{pane_current_command}')
        core('sidebar-control', 'toggle', window)
        tmux('select-pane', '-t', other)
        wait_name('cmd:tail')
        tmux('select-pane', '-t', main)
        wait_name('cmd:sleep')
        core('sidebar-control', 'toggle', window)
        assert tmux('show-option', '-wv', '-t', window, 'automatic-rename-format') == 'cmd:#{pane_current_command}'
        core('sidebar-control', 'toggle', window)
        tmux('set-option', '-g', '@sidebar_width', '26')
        tmux('set-option', '-g', '@sidebar_main_min_width', '80')
        subprocess.run([str(repo/'bin/workbench-responsive-zoom'), '160', window], env=env, check=True)
        subprocess.run([str(repo/'bin/workbench-responsive-zoom'), '80', window], env=env, check=True)
        assert tmux('display-message', '-p', '-t', window, '#{window_zoomed_flag}') == '1'
        subprocess.run([str(repo/'bin/workbench-responsive-zoom'), '160', window], env=env, check=True)
        assert tmux('display-message', '-p', '-t', window, '#{window_zoomed_flag}') == '0'
        sidebar = tmux('list-panes', '-t', window, '-f', '#{==:#{@pane_role},sidebar}', '-F', '#{pane_id}')
        # A legacy frozen window must respect Workbench's new stable-name policy.
        tmux('set-option', '-g', '@workbench-window-automatic-rename', 'off')
        tmux('set-option', '-w', '-t', window, 'automatic-rename', 'off')
        tmux('set-option', '-w', '-t', window, '@workbench_saved_automatic_rename', 'on')
        core('sidebar-control', 'maintain', window)
        assert tmux('show-option', '-wv', '-t', window, 'automatic-rename') == 'off'
        # Agent-assigned task names stay fixed through title and focus changes.
        tmux('rename-window', '-t', window, 'Stable task')
        title(main, 'Agent status changed')
        tmux('select-pane', '-t', sidebar)
        title(sidebar, 'Sidebar title changed')
        time.sleep(1)
        wait_name('Stable task')
        tmux('select-pane', '-t', other)
        core('sidebar-control', 'maintain', window)
        wait_name('Stable task')
        assert tmux('show-option', '-wv', '-t', window, 'automatic-rename') == 'off'

    finally:
        subprocess.run(['tmux', '-S', socket, 'kill-server'], capture_output=True)
print('sidebar naming and responsive zoom passed')
