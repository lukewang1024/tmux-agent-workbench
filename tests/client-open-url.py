"""Exercise native client URL opening over the real framed SSH client loop."""
import json
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest


FAKE_SSH = runpy.run_path(str(Path(__file__).with_name("client-terminals.py")))["FAKE_SSH"]
FAKE_SSH = FAKE_SSH.replace("heartbeat_seconds=15", "heartbeat_seconds=1")
FAKE_SSH = FAKE_SSH.replace(
    'if message["type"] == "heartbeat":',
    '''if message["type"] == "open_url_result":
            (root / (case + ".result")).write_text(json.dumps(message))
        if message["type"] == "heartbeat":
            if not (root / (case + ".sent")).exists():
                (root / (case + ".sent")).touch()
                write(dict(type="open_url", version=2, request_id="url-test", url=os.environ["TEST_URL"]))''',
)


class ClientOpenUrl(unittest.TestCase):
    def test_native_openers_return_success_failure_and_timeout_to_the_server(self):
        repo = Path(__file__).resolve().parents[1]
        binary = Path(os.environ.get("WORKBENCH_TEST_BINARY", str(repo / "target/debug/tmux-agent-workbench")))
        for case in ("termux", "desktop", "wsl", "denied", "hung"):
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="wo-", dir="/tmp") as directory:
                root = Path(directory)
                ssh = root / "ssh"
                ssh.write_text("#!" + sys.executable + "\n" + FAKE_SSH)
                ssh.chmod(0o700)
                helper_name = ("powershell.exe" if case == "wsl" else
                               ("open" if sys.platform == "darwin" else "xdg-open") if case == "desktop" else
                               "termux-open-url")
                helper = root / helper_name
                helper.write_text("#!" + sys.executable + "\nimport json,os,pathlib,sys,time\n"
                    "pathlib.Path(os.environ['PROBE_ROOT'],'opened').write_text(json.dumps(sys.argv[1:]))\n"
                    + ("time.sleep(60)\n" if case == "hung" else "")
                    + ("sys.exit(1)\n" if case == "denied" else ""))
                helper.chmod(0o700)
                url = "https://example.com/a?x=1&y=$(whoami)&q='quoted'#part"
                env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                           PROBE_ROOT=str(root), PROBE_CASE=case, TEST_URL=url)
                env.pop("TERMUX_VERSION", None)
                env.pop("WSL_DISTRO_NAME", None)
                if case == "wsl": env["WSL_DISTRO_NAME"] = "test"
                elif case != "desktop": env["TERMUX_VERSION"] = "test"
                for name in ("XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
                    folder = root / name
                    folder.mkdir()
                    env[name] = str(folder)
                master, slave = os.openpty()
                process = subprocess.Popen([str(binary), "client", "attach", "fake-host"],
                    env=env, stdin=slave, stdout=slave, stderr=subprocess.PIPE)
                try:
                    deadline = time.monotonic() + 8
                    result_path = root / (case + ".result")
                    while not result_path.exists() and time.monotonic() < deadline:
                        if process.poll() is not None:
                            self.fail(process.communicate()[1].decode())
                        time.sleep(.02)
                    self.assertTrue(result_path.exists(), "client never acknowledged URL opening")
                    result = json.loads(result_path.read_text())
                    hello = json.loads((root / (case + ".hello")).read_text())
                    self.assertIn("open-url", hello["capabilities"])
                    self.assertEqual(result["request_id"], "url-test")
                    args = json.loads((root / "opened").read_text())
                    if case == "wsl": self.assertIn(url.replace("'", "''"), args[-1])
                    else: self.assertEqual(args, [url])
                    if case in ("denied", "hung"):
                        self.assertTrue(result["error"])
                    else: self.assertIsNone(result["error"])
                    self.assertIsNone(process.poll(), "URL action disconnected the SSH session")
                finally:
                    if process.poll() is None: process.terminate()
                    for path in root.glob("*.pid"):
                        try: os.kill(int(path.read_text()), signal.SIGKILL)
                        except ProcessLookupError: pass
                    try:
                        process.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.communicate(timeout=5)
                    os.close(master)
                    os.close(slave)


if __name__ == "__main__":
    unittest.main()
