"""Verify the read-only snapshot against a real, disposable tmux server."""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def main():
    core, agent = (str(Path(path).resolve()) for path in sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="wb-observe-", dir="/tmp") as temporary:
        root = Path(temporary)
        socket = str(root / "tmux.sock")
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        env = {
            **os.environ,
            "TMUX_AGENT_WORKBENCH_TMUX_SOCKET": socket,
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_RUNTIME_DIR": str(runtime),
            "XDG_STATE_HOME": str(root / "state"),
        }
        env.pop("TMUX", None)
        env.pop("TMUX_PANE", None)

        def tmux(*args):
            return subprocess.check_output(
                ["tmux", "-S", socket, *args], env=env, text=True, timeout=5
            ).strip()

        try:
            first = tmux("-f", "/dev/null", "new-session", "-d", "-s", "observe",
                         "-P", "-F", "#{pane_id}", agent)
            second = tmux("split-window", "-d", "-P", "-F", "#{pane_id}", "-t", first, agent)
            tmux("set-option", "-p", "-t", first, "@workbench_agent_actions", "before")
            layout = tmux("list-panes", "-a", "-F",
                          "#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_active}|#{window_layout}")

            def observe():
                return json.loads(subprocess.check_output(
                    [core, "snapshot", "--observe", "--json"], env=env, text=True, timeout=15
                ))

            snapshots = [observe(), observe()]
            first_identity = None
            for snapshot in snapshots:
                assert snapshot["schema_version"] == 1, snapshot
                assert len(snapshot["sessions"]) == 1, snapshot
                agents = {item["target"]["pane_id"]: item for item in snapshot["agents"]}
                assert set(agents) == {first, second}, snapshot
                assert all(item["kind"] == "codex" for item in agents.values()), snapshot
                identity = {pane: item["instance_id"] for pane, item in agents.items()}
                if first_identity is not None:
                    assert identity == first_identity, snapshots
                first_identity = identity

            assert layout == tmux(
                "list-panes", "-a", "-F",
                "#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_active}|#{window_layout}",
            )
            assert tmux("show-option", "-pqv", "-t", first, "@workbench_agent_actions") == "before"
            assert not list(runtime.iterdir()), list(runtime.iterdir())
            assert not (root / "state").exists()
            print("PASS read-only Workbench snapshot preserves tmux layout, options and Agent identity")
        finally:
            subprocess.run(["tmux", "-S", socket, "kill-server"], env=env, check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)


if __name__ == "__main__":
    main()
