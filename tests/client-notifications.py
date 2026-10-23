"""Real attach CLI: failed/hung notification helpers must not interrupt heartbeats."""
import json
import os
from pathlib import Path
import runpy
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

FAKE_SSH = runpy.run_path(str(Path(__file__).with_name("client-terminals.py")))["FAKE_SSH"]
FAKE_SSH = FAKE_SSH.replace('write(dict(type="heartbeat_ack", version=2, events=0))', '''
            count = int((root / (case + ".beats")).read_text()) + 1 if (root / (case + ".beats")).exists() else 1
            (root / (case + ".beats")).write_text(str(count))
            write(dict(type="event_delivery", version=2, event_id="notification-probe",
                category="completion", title="Test", body="Test", target=dict(
                    session_id="$1", session_name="test", window_id="@1", window_index=0,
                    window_name="test", pane_id="%1", pane_index=0)))
            write(dict(type="heartbeat_ack", version=2, events=1))
''')

class OptionalNotifications(unittest.TestCase):
    def test_failures_and_timeouts_preserve_both_ssh_channels(self):
        repo = Path(__file__).resolve().parents[1]
        target = Path(os.environ.get("CARGO_TARGET_DIR", str(repo / "target")))
        binary = Path(os.environ.get("WORKBENCH_TEST_BINARY", str(target / "debug/tmux-agent-workbench")))
        with tempfile.TemporaryDirectory(prefix="wn-", dir="/tmp" if sys.platform == "darwin" else None) as directory:
            root = Path(directory)
            processes, fds = [], []
            try:
                for case, script in [("denied", "exit 1"), ("hung", "sleep 60 & wait")]:
                    folder = root / case
                    folder.mkdir()
                    ssh = folder / "ssh"
                    ssh.write_text("#!" + sys.executable + "\n" + FAKE_SSH)
                    ssh.chmod(0o700)
                    helper = folder / ("osascript" if sys.platform == "darwin" else "notify-send")
                    helper.write_text("#!" + shutil.which("sh") + "\nprintf called > '" + str(folder / "called") + "'\n" + script + "\n")
                    helper.chmod(0o700)
                    env = dict(os.environ, PATH=str(folder) + os.pathsep + os.environ["PATH"],
                               PROBE_ROOT=str(folder), PROBE_CASE=case)
                    env.pop("TERMUX_VERSION", None)
                    env.pop("WSL_DISTRO_NAME", None)
                    for name in ("XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
                        path = folder / name
                        path.mkdir()
                        env[name] = str(path)
                    master, slave = os.openpty()
                    fds.extend([master, slave])
                    process = subprocess.Popen([str(binary), "client", "attach", "fake-host"],
                        env=env, stdin=slave, stdout=slave, stderr=subprocess.PIPE)
                    processes.append((case, folder, process))
                deadline = time.monotonic() + 40
                while time.monotonic() < deadline:
                    for case, folder, process in processes:
                        if process.poll() is not None:
                            self.fail(case + " disconnected: " + process.communicate()[1].decode())
                    if all((folder / (case + ".beats")).exists() and
                           int((folder / (case + ".beats")).read_text()) >= 2
                           for case, folder, process in processes):
                        break
                    time.sleep(.05)
                else:
                    self.fail("second heartbeat did not arrive")
                for case, folder, process in processes:
                    self.assertTrue((folder / "called").exists())
                    os.kill(int((folder / (case + ".pty.pid")).read_text()), signal.SIGTERM)
                    _, error = process.communicate(timeout=5)
                    self.assertIn(b"notification skipped", error)
                    self.assertIn(b"timed out" if case == "hung" else b"notification delivery failed", error)
            finally:
                for case, folder, process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                    for path in folder.glob("*.pid"):
                        try:
                            os.kill(int(path.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                for fd in fds:
                    os.close(fd)

if __name__ == "__main__":
    unittest.main()
