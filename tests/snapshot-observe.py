"""Verify the read-only snapshot against a real, disposable tmux server."""
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
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

            def observe(*options):
                return json.loads(subprocess.check_output(
                    [core, "snapshot", "--observe", "--json", *options], env=env, text=True, timeout=15
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

            window, session = tmux("display-message", "-p", "-t", first,
                                   "#{window_id} #{session_id}").split()
            tmux("set-option", "-wt", window, "@agent_focus", "1")
            tmux("set-option", "-t", session, "@workbench_session_focus", "1")
            view = observe("--sidebar")
            assert "_workbench_surface" not in view, view
            sidebar = view["sidebar"]
            assert sidebar["sessionRows"] == sidebar["pinnedSessions"] == [session], sidebar
            assert sidebar["pinnedAgents"][0]["window"] == window, sidebar
            assert sidebar["agentRows"][0] == {"group": "observe"}, sidebar
            assert {item["id"] for item in sidebar["agentRows"][1:]} == set(first_identity.values()), sidebar
            assert sidebar["colors"]["working"] == "#e5c07b", sidebar

            assert layout == tmux(
                "list-panes", "-a", "-F",
                "#{session_id}|#{window_id}|#{pane_id}|#{pane_pid}|#{pane_active}|#{window_layout}",
            )
            assert tmux("show-option", "-pqv", "-t", first, "@workbench_agent_actions") == "before"

            def pane_key(pane):
                return tmux("display-message", "-p", "-t", pane,
                            "#{pid}.#{start_time}.#{pane_id}.#{pane_pid}").replace("%", "")

            def close(pane, identity=None, key=None):
                args = [core, "close-target", "--observe", "--pane", pane,
                        "--key", key or pane_key(pane)]
                if identity is not None:
                    args.extend(["--agent", identity])
                return subprocess.run(args, env=env, text=True, capture_output=True, timeout=5)

            pane = tmux("split-window", "-dPF", "#{pane_id}", "-t", first, "exec /bin/sh -i")
            tmux("send-keys", "-t", pane, "-l",
                 "printf '\\n› Ask Codex to do anything\\n'; " + shlex.quote(agent))
            tmux("send-keys", "-t", pane, "Enter")
            for _ in range(40):
                row = next((item for item in observe()["agents"] if item["target"]["pane_id"] == pane), None)
                if row and row["display_state"] == "idle":
                    break
                time.sleep(.05)
            assert row and row["display_state"] == "idle", row
            target = row["instance_id"]
            assert close(pane, first_identity[first]).returncode != 0
            assert close(pane, key="1.2.3.4").returncode != 0
            for title in ("⠋ Working", "Action Required"):
                tmux("select-pane", "-t", pane, "-T", title)
                for identity in (target, None):
                    refused = close(pane, identity)
                    assert refused.returncode != 0 and "Only idle or done" in refused.stderr, refused
            tmux("select-pane", "-t", pane, "-T", "Idle")
            closed = close(pane, target)
            assert closed.returncode == 0, closed.stderr
            for _ in range(40):
                if all(item["target"]["pane_id"] != pane for item in observe()["agents"]):
                    break
                time.sleep(.05)
            else:
                raise AssertionError("the closed agent remained in the pane")
            assert tmux("display-message", "-p", "-t", pane, "#{pane_id}") == pane
            tmux("set-option", "-pt", pane, "@pane_role", "sidebar")
            assert close(pane).returncode != 0
            tmux("set-option", "-pu", "-t", pane, "@pane_role")
            closed = close(pane)
            assert closed.returncode == 0, closed.stderr
            assert set(tmux("list-panes", "-aF", "#{pane_id}").splitlines()) == {first, second}
            assert not list(runtime.iterdir()), list(runtime.iterdir())
            assert not (root / "state").exists()
            print("PASS read-only inventory and guarded close target exact live tmux panes")
        finally:
            subprocess.run(["tmux", "-S", socket, "kill-server"], env=env, check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)


if __name__ == "__main__":
    main()
