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
