#!/usr/bin/env python3
"""Assert real gate descendants die, including detached groups and killed owners."""
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name("disposable.py")
GRANDCHILD = r"""
import os, pathlib, signal, socket, sys
os.setsid()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
root = pathlib.Path(os.environ['MYWHISPER_AUDIT_ROOT'])
connection = socket.socket(socket.AF_UNIX)
connection.connect(sys.argv[1])
connection.sendall(str(root).encode() + b'\n')
while True:
    if connection.recv(1) == b'x':
        (root / 'exit').touch()
"""
LAUNCHER = r"""
import os, pathlib, subprocess, sys, time
root = pathlib.Path(os.environ['MYWHISPER_AUDIT_ROOT'])
subprocess.Popen([sys.executable, '-c', sys.argv[2], sys.argv[1]],
                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                 stderr=subprocess.DEVNULL)
deadline = time.monotonic() + 5
while not (root / 'exit').exists():
    if time.monotonic() >= deadline:
        raise RuntimeError('private lifecycle barrier timed out')
    time.sleep(.005)
sys.exit(int(sys.argv[3]))
"""


class DisposableOwnership(unittest.TestCase):
    def run_case(self, status, kill_runner=False, timeout_runner=False):
        with tempfile.TemporaryDirectory(prefix="mywhisper-runner-owner-audit.") as directory:
            root = Path(directory)
            root.chmod(0o700)
            (root / ".disposable").touch(mode=0o600)
            pidfd = None
            scope = None
            connection = None
            with (root / "runner.log").open("wb") as log, socket.socket(socket.AF_UNIX) as listener:
                listener.bind(str(root / "socket"))
                listener.listen()
                listener.settimeout(5)
                command = [
                    sys.executable, str(RUNNER), "MYWHISPER_AUDIT_ROOT", "lifecycle",
                    sys.executable, "-c", LAUNCHER, str(root / "socket"), GRANDCHILD, str(status),
                ]
                if timeout_runner:
                    wrapper = "import runpy, sys; sys.argv = sys.argv[1:]; sys.exit(runpy.run_path(sys.argv[0])['main'](timeout=1))"
                    command = [sys.executable, "-c", wrapper, *command[1:]]
                owner = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
                try:
                    # SO_PEERCRED gives the host PID even inside a PID namespace.
                    # Retain its pidfd before allowing exit; never signal a PID
                    # read from a stale file or the namespace's local PID.
                    connection, _ = listener.accept()
                    connection.settimeout(2)
                    pid, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                    self.assertEqual(uid, os.geteuid())
                    pidfd = os.pidfd_open(pid)
                    candidate = Path(connection.recv(4096).decode().strip())
                    self.assertEqual(candidate.parent, Path(tempfile.gettempdir()))
                    self.assertTrue(candidate.name.startswith("mywhisper-lifecycle-audit."))
                    self.assertTrue((candidate / ".disposable").is_file())
                    self.assertEqual(candidate.stat().st_uid, os.geteuid())
                    self.assertEqual(candidate.stat().st_mode & 0o777, 0o700)
                    scope = candidate
                    self.assertFalse(select.select([pidfd], [], [], 0)[0])
                    if kill_runner:
                        owner.kill()
                    elif not timeout_runner:
                        connection.sendall(b'x')
                    actual = owner.wait(timeout=5)
                    self.assertEqual(actual, -signal.SIGKILL if kill_runner else 1 if timeout_runner else status)
                    if timeout_runner:
                        self.assertIn("TimeoutExpired", (root / "runner.log").read_text())
                    self.assertTrue(select.select([pidfd], [], [], 1)[0], "gate left its detached, SIGTERM-ignoring grandchild alive")
                finally:
                    if scope is not None and scope.exists():
                        (scope / "exit").touch(mode=0o600)
                    if pidfd is not None:
                        try:
                            signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        os.close(pidfd)
                    if owner.poll() is None:
                        owner.kill()
                        owner.wait(timeout=5)
                    if connection is not None:
                        connection.close()
                    # SIGKILL cannot run Python's directory destructor. Clean
                    # only the exact scope validated before killing its owner.
                    if scope is not None and scope.exists():
                        shutil.rmtree(scope)

    def test_success_cleans_detached_descendants(self):
        self.run_case(0)

    def test_failure_cleans_detached_descendants(self):
        self.run_case(7)

    def test_killed_runner_cleans_detached_descendants(self):
        self.run_case(0, kill_runner=True)

    def test_timeout_cleans_detached_descendants(self):
        self.run_case(0, timeout_runner=True)


if __name__ == "__main__":
    unittest.main()
