"""Exercise client serve against a daemon with deliberately delayed updates."""
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid

class DaemonTimeouts(unittest.TestCase):
    def test_slow_bookkeeping_does_not_close_control_channel(self):
        repo = Path(__file__).resolve().parents[1]
        target = Path(os.environ.get("CARGO_TARGET_DIR", str(repo / "target")))
        binary = os.environ.get("WORKBENCH_TEST_BINARY", str(target / "debug/tmux-agent-workbench"))
        with tempfile.TemporaryDirectory(prefix="wd-", dir="/tmp" if sys.platform == "darwin" else None) as directory:
            root = Path(directory)
            env = dict(os.environ)
            for name in ("XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
                folder = root / name
                folder.mkdir()
                env[name] = str(folder)
            # Keep the runtime path below the Unix socket fallback threshold.
            env["XDG_RUNTIME_DIR"] = str(root)
            tmux_socket = str(root / "tmux.sock")
            env["TMUX_AGENT_WORKBENCH_TMUX_SOCKET"] = tmux_socket
            runtime = root / "tmux-agent-workbench"
            runtime.mkdir(mode=0o700)
            key = hashlib.sha256(tmux_socket.encode()).hexdigest()[:16]
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(runtime / ("daemon-" + key + ".sock")))
            listener.listen()
            listener.settimeout(.1)
            stop = threading.Event()
            ready = threading.Event()
            process = None
            workers = []
            delayed = set()
            endpoint = str(uuid.uuid4())

            def respond(stream):
                with stream:
                    try:
                        request = json.loads(stream.makefile("rb").readline())
                        method = request["method"]
                        if method == "daemon.status":
                            ready.wait(5)
                            version = next(line.split('"')[1] for line in (repo / "Cargo.toml").read_text().splitlines() if line.startswith("version ="))
                            result = dict(engine_version=version, runtime_revision=1, pid=process.pid)
                        elif method == "client.register":
                            time.sleep(2.5)  # startup must also survive a busy scan
                            result = dict(endpoint_id=endpoint, attachment_token=str(uuid.uuid4()))
                        else:
                            if method in ("client.heartbeat", "client.focus", "client.accepted") and method not in delayed:
                                delayed.add(method)
                                time.sleep(2.5)
                            result = dict(events=[])
                        response = dict(protocol_version=1, id=request["id"], ok=True, result=result)
                        stream.sendall((json.dumps(response) + "\n").encode())
                    except (BrokenPipeError, ConnectionResetError):
                        pass

            def serve():
                while not stop.is_set():
                    try:
                        stream, _ = listener.accept()
                    except socket.timeout:
                        continue
                    worker = threading.Thread(target=respond, args=(stream,))
                    workers.append(worker)
                    worker.start()

            server = threading.Thread(target=serve)
            server.start()
            process = subprocess.Popen([binary, "client", "serve"], env=env,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            ready.set()

            def send(message_type, **fields):
                payload = json.dumps(dict(type=message_type, version=2, **fields)).encode()
                process.stdin.write(struct.pack(">I", len(payload)) + payload)
                process.stdin.flush()

            def receive():
                self.assertTrue(select.select([process.stdout], [], [], 5)[0], "control reply timed out")
                header = process.stdout.read(4)
                if len(header) != 4:
                    self.fail("control channel closed: " + process.stderr.read().decode())
                return json.loads(process.stdout.read(struct.unpack(">I", header)[0]))

            def heartbeat():
                send("heartbeat", activity_unix_ms=int(time.time() * 1000))
                self.assertEqual(receive()["type"], "heartbeat_ack")
                self.assertIsNone(process.poll())

            try:
                send("hello", device_id=str(uuid.uuid4()), device_label="timeout-test", kind="termux", capabilities=[])
                self.assertEqual(receive()["type"], "welcome")
                heartbeat()  # daemon times out, but transport still acknowledges
                heartbeat()  # next daemon request succeeds
                send("focus_result", focused=False, active_pane=None)
                heartbeat()
                send("event_accepted", event_id="timeout-test")
                heartbeat()
                send("goodbye")
                _, error = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0, error.decode())
                self.assertEqual(error, b"", "optional failures must not paint over the terminal")
                log = root / "XDG_STATE_HOME" / "tmux-agent-workbench" / ("daemon-" + key + ".log")
                diagnostic = log.read_bytes()
                self.assertEqual(log.stat().st_mode & 0o777, 0o600)
                for method in ("client.heartbeat", "client.focus", "client.accepted"):
                    self.assertIn((method + " skipped").encode(), diagnostic)
                    self.assertIn(method, delayed)
            finally:
                if process.poll() is None:
                    process.kill()
                if not process.stdout.closed:
                    process.communicate(timeout=5)
                stop.set()
                server.join(timeout=2)
                listener.close()
                for worker in workers:
                    worker.join(timeout=4)

if __name__ == "__main__":
    unittest.main()
