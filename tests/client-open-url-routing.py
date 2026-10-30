"""Real tmux, daemon, control frames and CLI receipts, isolated from live clients."""
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import time
import uuid

from daemon_fixture import running_daemon


core = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix="wo-route-", dir="/tmp") as directory:
    root = Path(directory)
    socket = str(root / "tmux.sock")
    env = dict(os.environ, TERM="xterm-256color", TMUX_AGENT_WORKBENCH_TMUX_SOCKET=socket)
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    for key in ("XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR"):
        env[key] = str(root / key)
    connections = []

    def tmux(*args):
        return subprocess.check_output(["tmux", "-S", socket, *args], env=env, text=True).strip()

    def write(channel, message):
        payload = json.dumps(dict(version=2, **message)).encode()
        channel.stdin.write(struct.pack(">I", len(payload)) + payload)
        channel.stdin.flush()

    def read(channel):
        def exact(size):
            data = b""
            deadline = time.monotonic() + 5
            while len(data) < size:
                timeout = deadline - time.monotonic()
                assert timeout > 0 and select.select([channel.stdout], [], [], timeout)[0], "control read timed out"
                part = os.read(channel.stdout.fileno(), size - len(data))
                assert part, "control channel closed"
                data += part
            return data
        return json.loads(exact(struct.unpack(">I", exact(4))[0]))

    def connect(capabilities):
        channel = subprocess.Popen([core, "client", "serve"], env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        write(channel, dict(type="hello", device_id=str(uuid.uuid4()), terminal_id=str(uuid.uuid4()),
                            device_label="test", kind="termux", capabilities=capabilities))
        welcome = read(channel)
        before = set(tmux("list-clients", "-F", "#{client_name}").splitlines())
        pid, master = pty.fork()
        if pid == 0:
            os.execvpe(core, [core, "client", "attach-pty", "--bind", welcome["attachment_token"], "--session", "probe"],
                       dict(env, SSH_TTY=os.ttyname(0)))
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            added = set(tmux("list-clients", "-F", "#{client_name}").splitlines()) - before
            if added:
                connection = (pid, master, next(iter(added)), channel)
                connections.append(connection)
                return connection
            time.sleep(.02)
        raise AssertionError("client did not attach")

    def poll(channel):
        write(channel, dict(type="heartbeat", activity_unix_ms=int(time.time() * 1000)))
        actions = []
        while True:
            message = read(channel)
            if message["type"] == "heartbeat_ack": return actions
            if message["type"] == "open_url": actions.append(message)

    try:
        tmux("-f", "/dev/null", "new-session", "-d", "-s", "probe", "sleep 120")
        with running_daemon(core, env):
            phone = connect(["open-url"])
            older = connect([])
            url = "https://example.com/a?x=1&y=2#part"
            rejected = subprocess.run([core, "client", "open-url", "--client", older[2], url], env=env, capture_output=True, text=True)
            assert rejected.returncode != 0 and "update Workbench" in rejected.stderr
            for error in (None, "phone opener denied"):
                caller = subprocess.Popen([core, "client", "open-url", "--client", phone[2], url],
                                          env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    deadline = time.monotonic() + 5
                    actions = []
                    while time.monotonic() < deadline and not actions:
                        assert poll(older[3]) == [], "request leaked to another terminal"
                        actions = poll(phone[3])
                        if not actions: time.sleep(.03)
                    assert len(actions) == 1 and actions[0]["url"] == url
                    write(phone[3], dict(type="open_url_result", request_id=actions[0]["request_id"], error=error))
                    _, stderr = caller.communicate(timeout=5)
                    assert caller.returncode == (1 if error else 0), stderr
                    if error: assert error.encode() in stderr
                    assert poll(phone[3]) == [], "request was retried"
                finally:
                    if caller.poll() is None: caller.kill()
                    caller.communicate(timeout=5)
            print("PASS exact-client URL delivery, old-client rejection and native success/error receipts")
            for pid, master, client, channel in connections:
                tmux("detach-client", "-t", client)
                os.waitpid(pid, 0)
                os.close(master)
                write(channel, dict(type="goodbye"))
                channel.communicate(timeout=5)
            connections.clear()
    finally:
        subprocess.run(["tmux", "-S", socket, "kill-server"], env=env, capture_output=True)
        for pid, master, client, channel in connections:
            try: os.kill(pid, 9)
            except ProcessLookupError: pass
            try: os.waitpid(pid, 0)
            except ChildProcessError: pass
            try: os.close(master)
            except OSError: pass
            channel.kill()
            channel.communicate(timeout=5)
