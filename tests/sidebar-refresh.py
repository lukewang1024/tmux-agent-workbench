#!/usr/bin/env python3
"""Verify detached sidebar throttling and recovery using an isolated tmux server."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="wb-sidebar-refresh-") as root:
    env = os.environ.copy()
    for key, subdir in [("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"),
                        ("XDG_CACHE_HOME", "cache")]:
        env[key] = str(Path(root) / subdir)
    socket = str(Path(root) / "tmux.sock")
    env["TMUX_AGENT_WORKBENCH_TMUX_SOCKET"] = socket
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    tmux = ["tmux", "-S", socket]

    def run(args):
        return subprocess.check_output(args, env=env, text=True).strip()

    def accepted():
        return json.loads(run([binary, "daemon", "status"]))["ipc_accepted"]

    client = None
    try:
        run(tmux + ["-f", "/dev/null", "new-session", "-d", "-s", "audit", "-x", "100", "-y", "30"])
        run([binary, "daemon", "ensure"])
        pane = run(tmux + ["split-window", "-d", "-P", "-F", "#{pane_id}", binary + " sidebar"])
        time.sleep(1.5)
        before = accepted()
        time.sleep(3)
        hidden = accepted() - before - 1  # Exclude our status request.
        assert hidden <= 1, f"hidden sidebar still polls every second: {hidden}"
        assert run(tmux + ["display-message", "-p", "-t", pane, "#{pane_dead}"]) == "0"

        # Control-mode attachment makes the window visible without focusing sidebar.
        client = subprocess.Popen(tmux + ["-C", "attach-session", "-t", "audit"],
                                  env=env, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL, text=True)
        time.sleep(6)
        before = accepted()
        time.sleep(3)
        visible = accepted() - before - 1
        assert visible >= 2, f"visible sidebar did not resume: {visible}"
        print(f"PASS: snapshot requests per 3 seconds: hidden={hidden}, visible={visible}")
    finally:
        if client is not None:
            client.terminate()
            client.wait(timeout=5)
        subprocess.run(tmux + ["kill-server"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
