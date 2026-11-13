"""Standard-library TLS and PTY transport embedded by remote_terminal.rs."""

import argparse
import fcntl
import hmac
import json
import os
import pty
import select
import signal
import socket
import ssl
import stat
import struct
import subprocess
import sys
import termios
import threading
import time
import tty
import uuid
from urllib.parse import urlsplit

PROTOCOL = 2
MAX_HELLO = 4096
MAX_DATA = 65536
MAX_CONTROL = 1024
READY = b"\x00"
INPUT, OUTPUT = "input", "output"
CONTROL = "control"
AGENT, CLIENT = "agent", "client"


def exact(stream, length):
    chunks = []
    remaining = length
    while remaining:
        chunk = stream.recv(remaining) if hasattr(stream, "recv") else stream.read(remaining)
        if not chunk:
            raise EOFError("terminal stream closed")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def send_hello(stream, hello):
    data = json.dumps(hello, separators=(",", ":")).encode("utf-8")
    if not data or len(data) > MAX_HELLO:
        raise ValueError("invalid terminal hello")
    stream.sendall(struct.pack(">I", len(data)) + data)


def read_hello(stream):
    length = struct.unpack(">I", exact(stream, 4))[0]
    if not length or length > MAX_HELLO:
        raise ValueError("invalid terminal hello length")
    message = json.loads(exact(stream, length).decode("utf-8"))
    if message.get("version") != PROTOCOL:
        raise ValueError("unsupported terminal protocol")
    if message.get("role") not in (AGENT, CLIENT) or message.get("lane") not in (INPUT, OUTPUT, CONTROL):
        raise ValueError("invalid terminal role or lane")
    attachment = message.get("attachment", "")
    if message["lane"] == CONTROL:
        if message["role"] != AGENT or attachment:
            raise ValueError("invalid terminal control hello")
    elif not valid_attachment_id(attachment):
        raise ValueError("invalid terminal attachment id")
    return message


def valid_attachment_id(value):
    return (
        isinstance(value, str)
        and len(value) == 32
        and all(character in "0123456789abcdef" for character in value)
    )


def write_frame(stream, kind, payload=b""):
    if kind == 1:
        if not payload or len(payload) > MAX_DATA:
            raise ValueError("invalid terminal data length")
        stream.sendall(b"\x01" + struct.pack(">I", len(payload)) + payload)
    elif kind == 2:
        if len(payload) != 4:
            raise ValueError("invalid resize frame")
        stream.sendall(b"\x02" + payload)
    elif kind == 3:
        if len(payload) < 8 or len(payload) > 264:
            raise ValueError("invalid terminal init frame")
        stream.sendall(b"\x03" + payload)
    elif kind == 4:
        stream.sendall(b"\x04")
    else:
        raise ValueError("invalid terminal frame kind")


def read_frame(stream):
    kind = exact(stream, 1)[0]
    if kind == 1:
        length = struct.unpack(">I", exact(stream, 4))[0]
        if not length or length > MAX_DATA:
            raise ValueError("invalid terminal data frame length")
        return kind, exact(stream, length)
    if kind == 2:
        return kind, exact(stream, 4)
    if kind == 3:
        header = exact(stream, 8)
        length, session_length = struct.unpack(">HH", header[4:])
        if not length or length > 128 or session_length > 128:
            raise ValueError("invalid terminal type")
        term = exact(stream, length)
        session = exact(stream, session_length) if session_length else b""
        if any(byte < 32 or byte == 127 for byte in term):
            raise ValueError("invalid terminal type")
        if any(byte < 32 or byte == 127 for byte in session):
            raise ValueError("invalid session name")
        return kind, header + term + session
    if kind == 4:
        return kind, b""
    raise ValueError("unknown terminal frame")


def write_control(stream, attachment):
    payload = json.dumps(
        {"type": "attach", "attachment": attachment}, separators=(",", ":")
    ).encode("utf-8")
    if len(payload) > MAX_CONTROL:
        raise ValueError("terminal control message is too large")
    stream.sendall(b"\x05" + struct.pack(">I", len(payload)) + payload)


def read_control(stream):
    if exact(stream, 1) != b"\x05":
        raise ValueError("invalid terminal control frame")
    length = struct.unpack(">I", exact(stream, 4))[0]
    if not length or length > MAX_CONTROL:
        raise ValueError("invalid terminal control frame length")
    message = json.loads(exact(stream, length).decode("utf-8"))
    if message.get("type") != "attach" or not valid_attachment_id(message.get("attachment")):
        raise ValueError("invalid terminal attach request")
    return message["attachment"]


def load_config(path):
    with open(path, "r", encoding="utf-8") as source:
        return json.load(source)


def token_for(settings):
    path = settings["token_file"]
    info = os.stat(path)
    if stat.S_IMODE(info.st_mode) & 0o077:
        raise PermissionError("token file permissions must be 0600: " + path)
    with open(path, "r", encoding="utf-8") as source:
        token = source.read().strip()
    if len(token) < 32 or any(ord(ch) < 32 for ch in token):
        raise ValueError("invalid pairing token in " + path)
    return token


def tls_context(settings):
    context = ssl.create_default_context(ssl.Purpose.SERVER_AUTH, cafile=settings["ca_certificate"])
    context.check_hostname = True
    context.verify_mode = ssl.CERT_REQUIRED
    return context


def open_lane(settings, target, token, role, lane, attachment=""):
    endpoint = urlsplit("//" + settings["endpoint"])
    if not endpoint.hostname or not endpoint.port:
        raise ValueError("endpoint must be host:port")
    raw = socket.create_connection((endpoint.hostname, endpoint.port), timeout=10)
    secure = tls_context(settings).wrap_socket(raw, server_hostname=settings["server_name"])
    secure.settimeout(240)
    send_hello(secure, {
        "version": PROTOCOL,
        "target": target,
        "token": token,
        "role": role,
        "lane": lane,
        "attachment": attachment,
    })
    return secure


def ready(stream):
    if exact(stream, 1) != READY:
        raise ValueError("invalid stream ready marker")
    stream.settimeout(None)


def relay(source, destination):
    while True:
        chunk = source.recv(65536)
        if not chunk:
            return
        destination.sendall(chunk)


def stream_is_closed(stream):
    try:
        stream.setblocking(False)
        try:
            return not stream.recv(1)
        except (ssl.SSLWantReadError, ssl.SSLWantWriteError, BlockingIOError):
            return False
        except (OSError, ssl.SSLError):
            return True
    except (OSError, ssl.SSLError):
        return True
    finally:
        try:
            stream.setblocking(True)
        except (OSError, ssl.SSLError):
            pass


def serve(config_path):
    config = load_config(config_path)
    server = config.get("server")
    if not server:
        raise ValueError("TLS server is not configured; run client stream server-configure")
    if stat.S_IMODE(os.stat(server["private_key"]).st_mode) & 0o077:
        raise PermissionError("TLS private key permissions must be 0600: " + server["private_key"])
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(server["certificate"], server["private_key"])
    bind_host, bind_port = server["bind"].rsplit(":", 1)
    listener = socket.create_server((bind_host, int(bind_port)))
    pending = {}
    pending_lock = threading.Lock()
    controls = {}
    control_send_locks = {}
    attachments = {}
    print("tmux-agent-workbench: TLS reverse terminal listening on " + server["bind"], file=sys.stderr, flush=True)

    def cleanup_old_attachments(target, now):
        requests = attachments.get(target)
        if requests is None:
            return
        for attachment, state in list(requests.items()):
            if now - state["created"] > 300:
                requests.pop(attachment, None)
        if not requests:
            attachments.pop(target, None)

    def notify_agent(target, attachment):
        with pending_lock:
            control = controls.get(target)
            send_lock = control_send_locks.get(target)
        if control is None or send_lock is None:
            return
        try:
            with send_lock:
                write_control(control, attachment)
        except Exception:
            with pending_lock:
                if controls.get(target) is control:
                    controls.pop(target, None)
                    control_send_locks.pop(target, None)

    def handle_control(secure, target):
        with pending_lock:
            existing = controls.get(target)
            if existing is not None and not stream_is_closed(existing):
                raise PermissionError("duplicate Mac control stream")
            if existing is not None:
                controls.pop(target, None)
                control_send_locks.pop(target, None)
                existing.close()
            secure.sendall(READY)
            controls[target] = secure
            control_send_locks[target] = threading.Lock()
            cleanup_old_attachments(target, time.monotonic())
            queued = list(attachments.get(target, {}))
        for attachment in queued:
            notify_agent(target, attachment)
        while True:
            payload = secure.recv(1)
            if not payload:
                return
            raise ValueError("unexpected Mac control payload")

    def handle(raw):
        secure = None
        peer = None
        transferred = False
        target = None
        control_registered = False
        try:
            secure = context.wrap_socket(raw, server_side=True)
            message = read_hello(secure)
            target = message.get("target")
            settings = config.get("targets", {}).get(target)
            if not settings or not hmac.compare_digest(message.get("token", ""), token_for(settings)):
                raise PermissionError("invalid target or token")
            lane = message["lane"]
            role = message["role"]
            if lane == CONTROL:
                if role != AGENT:
                    raise PermissionError("only the Mac agent can open a control stream")
                control_registered = True
                handle_control(secure, target)
                return

            attachment = message["attachment"]
            key = (target, attachment, lane)
            request_agent = False
            with pending_lock:
                cleanup_old_attachments(target, time.monotonic())
                if role == CLIENT and attachment not in attachments.setdefault(target, {}):
                    attachments[target][attachment] = {"created": time.monotonic(), "paired": 0}
                    request_agent = True
                current = pending.get(key)
                if current is None:
                    current = {
                        "role": role,
                        "stream": secure,
                        "peer": None,
                        "event": threading.Event(),
                        "cancelled": False,
                    }
                    pending[key] = current
                    first = True
                else:
                    if current["role"] == role:
                        if not stream_is_closed(current["stream"]):
                            raise PermissionError("duplicate stream peer")
                        pending.pop(key, None)
                        current["cancelled"] = True
                        current["event"].set()
                        current["stream"].close()
                        current = {
                            "role": role,
                            "stream": secure,
                            "peer": None,
                            "event": threading.Event(),
                            "cancelled": False,
                        }
                        pending[key] = current
                        first = True
                    else:
                        current["peer"] = secure
                        transferred = True
                        pending.pop(key, None)
                        state = attachments.get(target, {}).get(attachment)
                        if state is not None:
                            state["paired"] += 1
                            if state["paired"] >= 2:
                                attachments[target].pop(attachment, None)
                        current["event"].set()
                        first = False
            if request_agent:
                notify_agent(target, attachment)
            if not first:
                return
            deadline = time.monotonic() + 180
            while not current["event"].wait(0.25):
                with pending_lock:
                    if current["cancelled"]:
                        return
                    if pending.get(key) is not current:
                        continue
                    if stream_is_closed(secure):
                        pending.pop(key, None)
                        current["cancelled"] = True
                        state = attachments.get(target, {}).get(attachment)
                        if state is not None and state["paired"] == 0:
                            attachments[target].pop(attachment, None)
                        raise EOFError("reverse terminal peer disconnected before pairing")
                    if time.monotonic() >= deadline:
                        pending.pop(key, None)
                        current["cancelled"] = True
                        state = attachments.get(target, {}).get(attachment)
                        if state is not None and state["paired"] == 0:
                            attachments[target].pop(attachment, None)
                        raise TimeoutError("timed out waiting for reverse terminal peer")
            if current["cancelled"]:
                return
            peer = current["peer"]
            if peer is None:
                raise RuntimeError("paired stream missing peer")
            secure.sendall(READY)
            peer.sendall(READY)
            if lane == INPUT:
                source, destination = (peer, secure) if role == AGENT else (secure, peer)
            else:
                source, destination = (secure, peer) if role == AGENT else (peer, secure)
            relay(source, destination)
        except Exception as error:
            print("tmux-agent-workbench: reverse stream rejected/closed: " + str(error), file=sys.stderr, flush=True)
        finally:
            if control_registered and target is not None:
                with pending_lock:
                    if controls.get(target) is secure:
                        controls.pop(target, None)
                        control_send_locks.pop(target, None)
            try:
                if secure and not transferred:
                    secure.close()
                elif not secure:
                    raw.close()
            except OSError:
                pass
            if peer:
                try:
                    peer.close()
                except OSError:
                    pass

    while True:
        raw, _ = listener.accept()
        threading.Thread(target=handle, args=(raw,), daemon=True).start()


def winsize(fd):
    try:
        data = fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\x00" * 8)
        rows, columns, _, _ = struct.unpack("HHHH", data)
        return max(1, rows), max(1, columns)
    except OSError:
        return 24, 80


def set_winsize(fd, rows, columns, pid=None):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", max(1, rows), max(1, columns), 0, 0))
    if pid:
        try:
            os.killpg(pid, signal.SIGWINCH)
        except ProcessLookupError:
            pass


def agent_connection(config, target, session, attachment):
    settings = config["targets"][target]
    token = token_for(settings)
    incoming = open_lane(settings, target, token, AGENT, INPUT, attachment)
    outgoing = open_lane(settings, target, token, AGENT, OUTPUT, attachment)
    ready(incoming)
    ready(outgoing)
    kind, init = read_frame(incoming)
    if kind != 3:
        raise ValueError("client did not send terminal parameters")
    rows, columns, term_length, session_length = struct.unpack(">HHHH", init[:8])
    if len(init) != 8 + term_length + session_length:
        raise ValueError("invalid terminal initialization frame")
    term = init[8:8 + term_length].decode("utf-8")
    requested_session = init[8 + term_length:8 + term_length + session_length].decode("utf-8")
    pid, master = pty.fork()
    if pid == 0:
        os.environ["TERM"] = term
        argv = ["tmux", "attach-session"]
        tmux_socket = config.get("tmux_socket")
        if not tmux_socket:
            try:
                tmux_socket = subprocess.check_output(
                    ["tmux", "display-message", "-p", "#{socket_path}"],
                    stderr=subprocess.DEVNULL,
                    text=True,
                ).strip()
            except Exception:
                tmux_socket = None
        if tmux_socket:
            argv[1:1] = ["-S", tmux_socket]
        chosen_session = requested_session or session
        if chosen_session:
            argv.extend(["-t", chosen_session])
        try:
            os.execvp("tmux", argv)
        except BaseException as error:
            os.write(2, ("tmux attach failed: " + str(error) + "\r\n").encode())
            os._exit(127)
    set_winsize(master, rows, columns, pid)
    disconnected = threading.Event()

    def receive_input():
        try:
            while True:
                frame, payload = read_frame(incoming)
                if frame == 1:
                    os.write(master, payload)
                elif frame == 2:
                    new_rows, new_columns = struct.unpack(">HH", payload)
                    set_winsize(master, new_rows, new_columns, pid)
                elif frame == 4:
                    disconnected.set()
                    return
                else:
                    raise ValueError("unexpected terminal init frame")
        except Exception:
            disconnected.set()

    receiver = threading.Thread(target=receive_input, daemon=True)
    receiver.start()
    try:
        while not disconnected.is_set():
            ready_fds, _, _ = select.select([master], [], [], 0.2)
            if ready_fds:
                try:
                    payload = os.read(master, 8192)
                except OSError:
                    payload = b""
                if not payload:
                    break
                write_frame(outgoing, 1, payload)
            waited, status = os.waitpid(pid, os.WNOHANG)
            if waited == pid:
                break
    finally:
        try:
            incoming.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            outgoing.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        if not disconnected.is_set():
            try:
                os.killpg(pid, signal.SIGHUP)
            except ProcessLookupError:
                pass
        try:
            waited, _ = os.waitpid(pid, os.WNOHANG)
            if waited == 0:
                os.killpg(pid, signal.SIGTERM)
                os.waitpid(pid, 0)
        except (ChildProcessError, ProcessLookupError):
            pass
        os.close(master)
        receiver.join(timeout=1)


def agent_session(config_path, target, session, attachment):
    config = load_config(config_path)
    if target not in config.get("targets", {}):
        raise ValueError("TLS target is not configured: " + target)
    if not valid_attachment_id(attachment):
        raise ValueError("invalid terminal attachment id")
    agent_connection(config, target, session, attachment)


def agent(config_path, target, session):
    config = load_config(config_path)
    if target not in config.get("targets", {}):
        raise ValueError("TLS target is not configured: " + target)
    executable = os.environ.get("TMUX_AGENT_WORKBENCH_EXECUTABLE")
    if not executable:
        raise ValueError("Workbench executable path is unavailable")
    children = {}
    while True:
        control = None
        try:
            settings = config["targets"][target]
            control = open_lane(settings, target, token_for(settings), AGENT, CONTROL)
            ready(control)
            print("tmux-agent-workbench: control stream connected for " + target, file=sys.stderr, flush=True)
            while True:
                attachment = read_control(control)
                for child_id, child in list(children.items()):
                    if child.poll() is not None:
                        children.pop(child_id, None)
                child = children.get(attachment)
                if child is not None and child.poll() is None:
                    continue
                command = [
                    executable,
                    "client",
                    "stream",
                    "agent-session",
                    target,
                    "--attachment",
                    attachment,
                ]
                if session:
                    command.extend(["--session", session])
                child = subprocess.Popen(command, close_fds=True)
                children[attachment] = child
                threading.Thread(target=child.wait, daemon=True).start()
        except Exception as error:
            print("tmux-agent-workbench: " + target + " control stream: " + str(error) + "; retrying", file=sys.stderr, flush=True)
        finally:
            if control is not None:
                try:
                    control.close()
                except OSError:
                    pass
        time.sleep(2)


def attach(config_path, target, session):
    if not os.isatty(0):
        raise ValueError("client stream attach requires a terminal")
    config = load_config(config_path)
    settings = config.get("targets", {}).get(target)
    if not settings:
        raise ValueError("TLS target is not configured: " + target)
    token = token_for(settings)
    attachment = uuid.uuid4().hex
    incoming = open_lane(settings, target, token, CLIENT, INPUT, attachment)
    outgoing = open_lane(settings, target, token, CLIENT, OUTPUT, attachment)
    print("waiting for Mac agent " + target + "...", file=sys.stderr, flush=True)
    ready(incoming)
    ready(outgoing)
    rows, columns = winsize(0)
    term = os.environ.get("TERM", "xterm-256color")
    term_bytes = term.encode("utf-8")
    if not term_bytes or len(term_bytes) > 128 or any(byte < 32 or byte == 127 for byte in term_bytes):
        raise ValueError("invalid TERM value")
    session_bytes = (session or "").encode("utf-8")
    if len(session_bytes) > 128 or any(byte < 32 or byte == 127 for byte in session_bytes):
        raise ValueError("invalid session name")
    finished = threading.Event()
    original_terminal = termios.tcgetattr(0)
    receiver = None

    def receive_output():
        try:
            while True:
                frame, payload = read_frame(outgoing)
                if frame == 1:
                    view = memoryview(payload)
                    while view:
                        view = view[os.write(1, view):]
                elif frame == 4:
                    return
                else:
                    raise ValueError("unexpected output frame")
        except Exception as error:
            if not finished.is_set():
                print("tmux-agent-workbench: output stream closed: " + str(error), file=sys.stderr, flush=True)
        finally:
            finished.set()

    previous = (rows, columns)
    try:
        # This process is running on the Devbox PTY created by the phone's
        # `ssh -t`. Keep it byte-oriented so key and mouse escape sequences
        # reach Mac tmux instead of being echoed or line-buffered by the PTY.
        tty.setraw(0, termios.TCSADRAIN)
        write_frame(
            incoming,
            3,
            struct.pack(">HHHH", rows, columns, len(term_bytes), len(session_bytes))
            + term_bytes
            + session_bytes,
        )
        receiver = threading.Thread(target=receive_output, daemon=True)
        receiver.start()
        while not finished.is_set():
            readable, _, _ = select.select([0], [], [], 0.2)
            if readable:
                payload = os.read(0, 8192)
                if not payload:
                    write_frame(incoming, 4)
                    break
                write_frame(incoming, 1, payload)
            current = winsize(0)
            if current != previous:
                previous = current
                write_frame(incoming, 2, struct.pack(">HH", *current))
    finally:
        finished.set()
        try:
            incoming.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            outgoing.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        if receiver is not None:
            receiver.join(timeout=1)
        try:
            termios.tcsetattr(0, termios.TCSADRAIN, original_terminal)
        except OSError:
            pass


def self_test():
    import io

    class MemoryStream:
        def __init__(self):
            self.buffer = io.BytesIO()

        def sendall(self, value):
            self.buffer.write(value)

        def recv(self, size):
            return self.buffer.read(size)

    stream = MemoryStream()
    write_frame(stream, 1, b"\x00\x1b[31m\xff")
    write_frame(stream, 2, struct.pack(">HH", 55, 132))
    term = b"xterm-256color"
    write_frame(stream, 3, struct.pack(">HHHH", 24, 80, len(term), 0) + term)
    stream.buffer.seek(0)
    assert read_frame(stream) == (1, b"\x00\x1b[31m\xff")
    assert read_frame(stream) == (2, struct.pack(">HH", 55, 132))
    assert read_frame(stream) == (3, struct.pack(">HHHH", 24, 80, len(term), 0) + term)
    print("remote-terminal protocol self-test passed")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", default=os.path.expanduser("~/.config/tmux-agent-workbench/remote-terminal.json"))
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("serve")
    for name in ("agent", "attach"):
        child = subparsers.add_parser(name)
        child.add_argument("--target", required=True)
        child.add_argument("--session")
    child = subparsers.add_parser("agent-session")
    child.add_argument("--target", required=True)
    child.add_argument("--attachment", required=True)
    child.add_argument("--session")
    subparsers.add_parser("self-test")
    args = parser.parse_args()
    if args.command == "serve":
        serve(args.config)
    elif args.command == "agent":
        agent(args.config, args.target, args.session)
    elif args.command == "agent-session":
        agent_session(args.config, args.target, args.session, args.attachment)
    elif args.command == "attach":
        attach(args.config, args.target, args.session)
    else:
        self_test()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        raise SystemExit(130)
    except Exception as error:
        print("tmux-agent-workbench: " + str(error), file=sys.stderr)
        raise SystemExit(1)
