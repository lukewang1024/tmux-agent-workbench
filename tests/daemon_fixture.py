"""Keep daemon state writers alive only inside their test workspace."""
from contextlib import contextmanager
import subprocess
import time


@contextmanager
def running_daemon(core, env):
    # `daemon ensure` detaches the process; `daemon stop` only acknowledges
    # shutdown. Own the process and reap it before removing its workspace.
    daemon = subprocess.Popen([core, 'daemon', 'run'], env=env,
                              stdout=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 10
        while True:
            assert daemon.poll() is None, 'daemon exited before becoming ready'
            status = subprocess.run([core, 'daemon', 'status'], env=env,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if status.returncode == 0:
                break
            if time.monotonic() >= deadline:
                raise AssertionError('daemon did not become ready')
            time.sleep(0.05)
        yield daemon
    finally:
        try:
            if daemon.poll() is None:
                subprocess.run([core, 'daemon', 'stop'], env=env, timeout=5,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            # Leave no writer behind, but still fail a hung shutdown.
            daemon.kill()
            daemon.wait(timeout=5)
            raise
        assert daemon.returncode == 0, f'daemon exited with {daemon.returncode}'
