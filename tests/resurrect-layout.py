#!/usr/bin/env python3
"""Exercise save hooks and layout restoration on an isolated tmux server."""
import os
from pathlib import Path
import re
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="wb-resurrect-") as root:
    socket = str(Path(root) / "tmux.sock")
    env = dict(os.environ, TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)

    def tmux(*args):
        return subprocess.check_output(
            ["tmux", "-S", socket, *args], env=env, text=True
        ).strip()

    def split(target, *args):
        return tmux("split-window", "-d", "-t", target, "-P", "-F", "#{pane_id}", *args)

    def layout(target):
        return tmux("display-message", "-p", "-t", target, "#{window_layout}")

    def snapshot(target, name):
        panes = tmux("list-panes", "-t", target, "-F",
                     "pane\t#{session_name}\t#{window_index}\t0\t:-\t#{pane_index}"
                     "\ttitle\t:/tmp\t#{pane_active}\tsh\t:")
        window = tmux("display-message", "-p", "-t", target, "-F",
                      "window\t#{session_name}\t#{window_index}\t:name\t0\t:-"
                      "\t#{window_layout}\toff")
        path = Path(root) / (name + ".txt")
        path.write_text(panes + "\n" + window + "\n")
        subprocess.run([str(repo / "bin/workbench-resurrect-save-hook"), str(path)],
                       env=env, check=True)
        rows = [line.split("\t") for line in path.read_text().splitlines()]
        saved = next(row[6] for row in rows if row[0] == "window")
        count = sum(row[0] == "pane" for row in rows)
        leaves = re.findall(r"\d+x\d+,\d+,\d+,(\d+)(?=[,}\]]|$)", saved)
        expected = tmux("list-panes", "-t", target, "-f",
                        "#{!=:#{@pane_role},sidebar}", "-F", "#{pane_id}").splitlines()
        assert set(leaves) == {pane[1:] for pane in expected}, (name, saved, expected)
        assert len(leaves) == count
        # Let tmux validate the checksum, geometry, and pane count, then verify
        # it actually restored every rectangle (pane IDs are server-assigned).
        dest = tmux("new-window", "-d", "-n", "restore", "-P", "-F", "#{window_id}")
        tmux("resize-window", "-t", dest, "-x", "240", "-y", "100")
        for _ in range(count - 1):
            split(dest, "-v")
            tmux("select-layout", "-t", dest, "even-vertical")
        tmux("select-layout", "-t", dest, saved)
        rectangles = lambda value: re.findall(r"(\d+x\d+,\d+,\d+),\d+(?=[,}\]]|$)", value)
        assert rectangles(layout(dest)) == rectangles(saved), name
        tmux("kill-window", "-t", dest)
        print("PASS", name, count, "panes")
        return saved

    try:
        tmux("-f", "/dev/null", "new-session", "-d", "-s", "audit", "-x", "240", "-y", "100")
        main = tmux("display-message", "-p", "-t", "audit", "#{pane_id}")
        other = split(main, "-h", "-l", "80")
        third = split(other, "-v", "-l", "30")
        original = layout(main)
        tmux("set-option", "-w", "-t", main, "@workbench_main_layout", original)
        sidebar = split(main, "-h", "-f", "-b", "-l", "26")
        tmux("set-option", "-p", "-t", sidebar, "@pane_role", "sidebar")
        snapshot(main, "nested-splits")
        added = split(main, "-v", "-l", "20")
        snapshot(main, "pane-added-with-stale-cache")
        tmux("kill-pane", "-t", third)
        before_resize = snapshot(main, "pane-removed-with-stale-cache")
        tmux("resize-pane", "-t", added, "-y", "35")
        changed = snapshot(main, "pane-resized")
        assert changed != before_resize
        tmux("resize-pane", "-Z", "-t", main)
        assert snapshot(main, "zoomed") == changed
        tmux("resize-pane", "-Z", "-t", main)
        # Sidebars can be nested after a user rearranges the layout.
        tmux("select-layout", "-t", main, "tiled")
        snapshot(main, "sidebar-in-tiled-layout")
        sidebar2 = split(other, "-v", "-l", "5")
        tmux("set-option", "-p", "-t", sidebar2, "@pane_role", "sidebar")
        snapshot(main, "multiple-sidebars")
        tmux("kill-pane", "-t", sidebar)
        tmux("kill-pane", "-t", sidebar2)
        assert snapshot(main, "no-sidebar-stale-cache") == layout(main)
        tmux("kill-pane", "-t", added)
        tmux("kill-pane", "-t", other)
        sidebar = split(main, "-h", "-l", "26")
        tmux("set-option", "-p", "-t", sidebar, "@pane_role", "sidebar")
        snapshot(main, "right-sidebar-single-main-pane")
    finally:
        subprocess.run(["tmux", "-S", socket, "kill-server"], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
