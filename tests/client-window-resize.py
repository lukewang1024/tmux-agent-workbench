#!/usr/bin/env python3
"""Exercise shared-session window switches from desktop and phone PTYs."""
from contextlib import ExitStack
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

from daemon_fixture import running_daemon

repo = Path(__file__).resolve().parents[1]
core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="wb-client-window-") as root, ExitStack() as resources:
    env = dict(os.environ, TERM="xterm-256color",
               TMUX_AGENT_WORKBENCH_TMUX_SOCKET=root + "/tmux.sock",
               TMUX_AGENT_WORKBENCH_BIN=core)
    for key in ("TMUX", "TMUX_PANE"):
        env.pop(key, None)
    for key in ("XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
        env[key] = str(Path(root) / key)
        Path(env[key]).mkdir()
    clients = []

    def tmux(*args):
        return subprocess.check_output(
            ["tmux", "-S", env["TMUX_AGENT_WORKBENCH_TMUX_SOCKET"], *args],
            env=env, text=True).strip()

    def drain():
        for process, master, slave in clients:
            while select.select([master], [], [], 0)[0]:
                try:
                    os.read(master, 65536)
                except OSError:
                    break

    def settled(index, width, height):
        deadline = time.monotonic() + 8
        while True:
            drain()
            value = tmux("display-message", "-p", "-t", "test:",
                         "#{window_index}|#{window_width}|#{window_height}|#{pane_width}|#{pane_height}")
            actual_index, ww, wh, pw, ph = map(int, value.split("|"))
            if (actual_index, ww, wh, pw, ph) == (index, width, height, width, height):
                return
            if time.monotonic() >= deadline:
                raise AssertionError((index, width, height, value))
            time.sleep(.03)

    try:
        tmux("-f", "/dev/null", "new-session", "-d", "-s", "test", "-x", "318", "-y", "76")
        resources.enter_context(running_daemon(core, env))
        tmux("set-option", "-gw", "window-size", "latest")
        tmux("set-option", "-gw", "aggressive-resize", "on")
        for index in (1, 2, 3):
            tmux("new-window", "-d", "-t", f"test:{index}")
            window = tmux("display-message", "-p", "-t", f"test:{index}", "#{window_id}")
            subprocess.run([core, "sidebar-control", "toggle", window, "--create-only"],
                           env=env, check=True, stdout=subprocess.DEVNULL)
        tmux("run-shell", str(repo / "workbench.tmux"))
        for width, height in ((318, 76), (63, 31)):
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
            process = subprocess.Popen(
                ["tmux", "-S", env["TMUX_AGENT_WORKBENCH_TMUX_SOCKET"], "attach-session", "-t", "test"],
                env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            clients.append((process, master, slave))
            deadline = time.monotonic() + 5
            while len(tmux("list-clients").splitlines()) != len(clients):
                drain()
                assert time.monotonic() < deadline, "client failed to attach"
                time.sleep(.03)
        for index in (1, 2, 3, 2, 1):
            os.write(clients[1][1], b"\x02" + str(index).encode())
            settled(index, 63, 30)
        print("PASS phone window switches resize the window and foreground pane together")

        # With window-size=latest, desktop activity (or a late resize hook)
        # can grow the shared window while the phone still views it. Every
        # sidebar entrypoint must keep the phone's viewport free of drawers.
        window = tmux("display-message", "-p", "-t", "test:", "#{window_id}")

        def maintain():
            subprocess.run([core, "sidebar-control", "maintain", window],
                           env=env, check=True, stdout=subprocess.DEVNULL)

        def assert_sidebar(expected):
            roles = tmux("list-panes", "-t", window, "-F", "#{@pane_role}").splitlines()
            assert ("sidebar" in roles) == expected, (expected, roles)

        for action in ("maintain", "ensure-all", "toggle"):
            tmux("resize-window", "-t", window, "-x", "318", "-y", "75")
            command = [core, "sidebar-control", action]
            if action != "ensure-all":
                command.append(window)
            if action == "toggle":
                command.append("--create-only")
            subprocess.run(command, env=env, check=True, stdout=subprocess.DEVNULL)
            assert_sidebar(False)
        print("PASS desktop-sized shared windows stay free of drawers while a phone is viewing")

        # Only clients viewing this exact window participate in its layout.
        # A phone elsewhere must not suppress the desktop sidebar.
        tmux("new-session", "-d", "-s", "phone-away")
        phone = os.ttyname(clients[1][2])
        tmux("switch-client", "-c", phone, "-t", "phone-away")
        maintain()
        assert_sidebar(True)
        print("PASS desktop drawer returns when the phone leaves the shared window")

        # Manual dismissal remains distinct from automatic narrow hiding.
        subprocess.run([core, "sidebar-control", "toggle", window],
                       env=env, check=True, stdout=subprocess.DEVNULL)
        assert_sidebar(False)
        tmux("switch-client", "-c", phone, "-t", "test")
        maintain()
        assert_sidebar(False)
        tmux("switch-client", "-c", phone, "-t", "phone-away")
        maintain()
        assert_sidebar(False)
        print("PASS responsive transitions preserve manual sidebar dismissal")
    finally:
        subprocess.run(["tmux", "-S", env["TMUX_AGENT_WORKBENCH_TMUX_SOCKET"], "kill-server"],
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for process, master, slave in clients:
            os.close(master)
            os.close(slave)
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
