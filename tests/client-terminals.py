"""Exercise the real attach CLI with two PTYs and a local fake SSH transport.

Run after cargo build: python3 tests/client-terminals.py
No remote host, live tmux server, or phone is used.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest


FAKE_SSH = r'''
import json, os, struct, sys, time, uuid
from pathlib import Path
root = Path(os.environ["PROBE_ROOT"])
case = os.environ["PROBE_CASE"]
role = "pty" if "-t" in sys.argv else "control"
(root / (case + "." + role + ".pid")).write_text(str(os.getpid()))
def read():
    size = sys.stdin.buffer.read(4)
    if not size:
        return None
    return json.loads(sys.stdin.buffer.read(struct.unpack(">I", size)[0]))
def write(message):
    data = json.dumps(message).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(data)) + data)
    sys.stdout.buffer.flush()
if role == "pty":
    time.sleep(60)
else:
    hello = read()
    (root / (case + ".hello")).write_text(json.dumps(hello))
    write(dict(type="welcome", version=2, endpoint_id=str(uuid.uuid4()),
               attachment_token=str(uuid.uuid4()), heartbeat_seconds=15))
    while True:
        message = read()
        if message is None or message["type"] == "goodbye":
            break
        if message["type"] == "heartbeat":
            write(dict(type="heartbeat_ack", version=2, events=0))
'''


class TerminalConnections(unittest.TestCase):
    def test_independent_tabs_duplicate_rejection_and_reconnect(self):
        binary = Path(__file__).resolve().parents[1] / "target/debug/tmux-agent-workbench"
        with tempfile.TemporaryDirectory(prefix="workbench-terminals-") as directory:
            root = Path(directory)
            ssh = root / "ssh"
            ssh.write_text("#!" + sys.executable + "\n" + FAKE_SSH)
            ssh.chmod(0o700)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                       PROBE_ROOT=str(root))
            for name in ("XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME",
                         "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
                path = root / name
                path.mkdir()
                env[name] = str(path)
            a_master, a_slave = os.openpty()
            b_master, b_slave = os.openpty()
            processes = []

            def start(case, slave):
                process = subprocess.Popen([str(binary), "client", "attach", "fake-host"],
                    env=dict(env, PROBE_CASE=case), stdin=slave, stdout=slave,
                    stderr=subprocess.PIPE)
                processes.append(process)
                return process

            def read_file(name, parse):
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    try:
                        return parse((root / name).read_text())
                    except (FileNotFoundError, ValueError):
                        time.sleep(.02)
                self.fail("timed out waiting for " + name)

            def stop(case, process):
                os.kill(read_file(case + ".pty.pid", int), signal.SIGTERM)
                process.communicate(timeout=5)
                for role in ("pty", "control"):
                    pid = read_file(case + "." + role + ".pid", int)
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid, 0)

            try:
                a = start("a", a_slave)
                b = start("b", b_slave)
                a_hello = read_file("a.hello", json.loads)
                b_hello = read_file("b.hello", json.loads)
                self.assertEqual(a_hello["device_id"], b_hello["device_id"])
                self.assertNotEqual(a_hello["terminal_id"], b_hello["terminal_id"])
                duplicate = start("duplicate", a_slave)
                _, error = duplicate.communicate(timeout=5)
                self.assertNotEqual(duplicate.returncode, 0)
                self.assertIn(b"cannot own this terminal", error)
                self.assertFalse((root / "duplicate.control.pid").exists())
                stop("a", a)
                self.assertIsNone(b.poll())
                reconnected = start("reconnected", a_slave)
                hello = read_file("reconnected.hello", json.loads)
                self.assertEqual(a_hello["terminal_id"], hello["terminal_id"])
                stop("reconnected", reconnected)
                stop("b", b)
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)
                for path in root.glob("*.pid"):
                    try:
                        os.kill(int(path.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                for fd in (a_master, a_slave, b_master, b_slave):
                    os.close(fd)


if __name__ == "__main__":
    unittest.main()
