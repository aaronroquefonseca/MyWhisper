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
import time
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
        with tempfile.TemporaryDirectory(
            prefix="mywhisper-runner-owner-audit."
        ) as directory:
            root = Path(directory)
            root.chmod(0o700)
            (root / ".disposable").touch(mode=0o600)
            pidfd = None
            scope = None
            connection = None
            with (
                (root / "runner.log").open("wb") as log,
                socket.socket(socket.AF_UNIX) as listener,
            ):
                listener.bind(str(root / "socket"))
                listener.listen()
                listener.settimeout(5)
                command = [
                    sys.executable,
                    str(RUNNER),
                    "MYWHISPER_AUDIT_ROOT",
                    "lifecycle",
                    sys.executable,
                    "-c",
                    LAUNCHER,
                    str(root / "socket"),
                    GRANDCHILD,
                    str(status),
                ]
                if timeout_runner:
                    wrapper = "import runpy, sys; sys.argv = sys.argv[1:]; sys.exit(runpy.run_path(sys.argv[0])['main'](timeout=1))"
                    command = [sys.executable, "-c", wrapper, *command[1:]]
                owner = subprocess.Popen(
                    command, stdin=subprocess.DEVNULL, stdout=log, stderr=log
                )
                try:
                    # SO_PEERCRED gives the host PID even inside a PID namespace.
                    # Retain its pidfd before allowing exit; never signal a PID
                    # read from a stale file or the namespace's local PID.
                    connection, _ = listener.accept()
                    connection.settimeout(2)
                    pid, uid, _ = struct.unpack(
                        "3i",
                        connection.getsockopt(
                            socket.SOL_SOCKET, socket.SO_PEERCRED, 12
                        ),
                    )
                    self.assertEqual(uid, os.geteuid())
                    pidfd = os.pidfd_open(pid)
                    candidate = Path(connection.recv(4096).decode().strip())
                    self.assertEqual(candidate.parent, Path(tempfile.gettempdir()))
                    self.assertTrue(
                        candidate.name.startswith("mywhisper-lifecycle-audit.")
                    )
                    self.assertTrue((candidate / ".disposable").is_file())
                    self.assertEqual(candidate.stat().st_uid, os.geteuid())
                    self.assertEqual(candidate.stat().st_mode & 0o777, 0o700)
                    scope = candidate
                    self.assertFalse(select.select([pidfd], [], [], 0)[0])
                    if kill_runner:
                        owner.kill()
                    elif not timeout_runner:
                        connection.sendall(b"x")
                    actual = owner.wait(timeout=5)
                    self.assertEqual(
                        actual,
                        -signal.SIGKILL
                        if kill_runner
                        else 1
                        if timeout_runner
                        else status,
                    )
                    if timeout_runner:
                        self.assertIn(
                            "TimeoutExpired", (root / "runner.log").read_text()
                        )
                    self.assertTrue(
                        select.select([pidfd], [], [], 1)[0],
                        "gate left its detached, SIGTERM-ignoring grandchild alive",
                    )
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

    def test_explicit_library_inputs_survive_private_tmp_without_becoming_writable(
        self,
    ):
        with tempfile.TemporaryDirectory(
            prefix="mywhisper-runner-input-audit."
        ) as directory:
            source = Path(directory)
            source.chmod(0o700)
            (source / ".disposable").touch(mode=0o600)
            (source / "synthetic-input").write_text("read-only synthetic input")
            env = os.environ.copy()
            env["LD_LIBRARY_PATH"] = str(source)
            code = r"""
import errno, os, pathlib
source = pathlib.Path(os.environ['LD_LIBRARY_PATH']) / 'synthetic-input'
assert source.read_text() == 'read-only synthetic input'
try:
    source.write_text('unauthorized fixture mutation')
except OSError as error:
    assert error.errno == errno.EROFS
else:
    raise AssertionError('external inputs became writable')
root = pathlib.Path(os.environ['MYWHISPER_AUDIT_ROOT'])
temporary = pathlib.Path(os.environ['TMPDIR'])
assert temporary.is_relative_to(root)
(temporary / 'owned-output').write_text('private synthetic output')
"""
            result = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    "MYWHISPER_AUDIT_ROOT",
                    "libraryinputs",
                    sys.executable,
                    "-c",
                    code,
                ],
                env=env,
                capture_output=True,
                timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(
                (source / "synthetic-input").read_text(), "read-only synthetic input"
            )


class DisposableTmpfsScratch(unittest.TestCase):
    probe = r"""
import os, pathlib
root = pathlib.Path(os.environ['MYWHISPER_AUDIT_ROOT'])
temporary = pathlib.Path(os.environ['TMPDIR'])
limit_probe = root / '.limitprobe'
assert temporary.is_relative_to(root)
mounts = [line.split() for line in open('/proc/mounts')]
assert 'MYWHISPER_AUDIT_TMPFS_TMP' not in os.environ
assert (root / '.disposable').is_file()
assert temporary.stat().st_uid == os.geteuid()
for variable, mode in [('HOME', 0o700), ('XDG_CONFIG_HOME', 0o700),
                       ('XDG_DATA_HOME', 0o700), ('XDG_RUNTIME_DIR', 0o700)]:
    assert pathlib.Path(os.environ[variable]).stat().st_mode & 0o777 == mode
overlay = [fields for fields in mounts if fields[1] == str(temporary)]
capped_mount = [fields for fields in mounts if fields[1] == str(limit_probe)]
if overlay and overlay[0][0] == 'tmpfs':
    assert temporary.stat().st_mode & 0o777 == 0o700
    scratch = os.statvfs(temporary)
    assert scratch.f_blocks * scratch.f_frsize == 2147483648
    assert capped_mount and capped_mount[0][0] == 'tmpfs'
    assert limit_probe.stat().st_mode & 0o777 == 0o700
    capped = os.statvfs(limit_probe)
    assert capped.f_blocks * capped.f_frsize == 1048576
    print('OVERLAY')
else:
    assert not capped_mount
    print('HOSTBACKED')
"""

    def run_probe(self, label, flag):
        env = os.environ.copy()
        if flag is None:
            env.pop("MYWHISPER_AUDIT_TMPFS_TMP", None)
        else:
            env["MYWHISPER_AUDIT_TMPFS_TMP"] = flag
        return subprocess.run(
            [
                sys.executable,
                str(RUNNER),
                "MYWHISPER_AUDIT_ROOT",
                label,
                sys.executable,
                "-c",
                self.probe,
            ],
            env=env,
            capture_output=True,
            timeout=30,
        )

    def test_tmpfs_scratch_is_private_tmpfs_roundtrip(self):
        result = self.run_probe("tmpfsscratch", "1")
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn("OVERLAY", result.stdout.decode())

    def test_limit_probe_enforces_enospc_without_gigabyte_allocs(self):
        code = r"""
import errno, os, pathlib
probe = pathlib.Path(os.environ['MYWHISPER_AUDIT_ROOT']) / '.limitprobe'
written = 0
try:
    with open(probe / 'fill', 'wb') as handle:
        while True:
            handle.write(b'z' * 65536)
            written += 65536
except OSError as error:
    assert error.errno == errno.ENOSPC, error
assert written < 2 * 1048576, written
print('ENOSPC-OK')
"""
        env = os.environ.copy()
        env["MYWHISPER_AUDIT_TMPFS_TMP"] = "1"
        result = subprocess.run(
            [
                sys.executable,
                str(RUNNER),
                "MYWHISPER_AUDIT_ROOT",
                "tmpfsenospc",
                sys.executable,
                "-c",
                code,
            ],
            env=env,
            capture_output=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn("ENOSPC-OK", result.stdout.decode())

    def test_default_tmpdir_has_no_overlay(self):
        result = self.run_probe("tmpfsdefault", None)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertIn("HOSTBACKED", result.stdout.decode())

    def test_invalid_flag_rejected(self):
        result = self.run_probe("tmpfsinvalid", "2")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("MYWHISPER_AUDIT_TMPFS_TMP", result.stderr.decode())

    def test_tmpfs_contents_hidden_from_host(self):
        label = f"tmpfsprivacy{os.getpid()}"
        child = (
            "import os, pathlib, time\n"
            "root = pathlib.Path(os.environ['MYWHISPER_AUDIT_ROOT'])\n"
            "temporary = pathlib.Path(os.environ['TMPDIR'])\n"
            "handle = os.open(temporary / 'secret', os.O_RDWR | os.O_CREAT, 0o600)\n"
            "os.write(handle, b'private'); os.fsync(handle); os.close(handle)\n"
            "with open(root / '.limitprobe/fill', 'wb') as probe:\n"
            "    probe.write(b'z' * 65536)\n"
            "(root / 'home/signal').touch()\n"
            "time.sleep(60)\n"
        )
        env = os.environ.copy()
        env["MYWHISPER_AUDIT_TMPFS_TMP"] = "1"
        owner = subprocess.Popen(
            [
                sys.executable,
                str(RUNNER),
                "MYWHISPER_AUDIT_ROOT",
                label,
                sys.executable,
                "-c",
                child,
            ],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        scope = None
        try:
            deadline = time.monotonic() + 15
            while scope is None:
                candidates = sorted(
                    Path(tempfile.gettempdir()).glob(f"mywhisper-{label}-audit.*")
                )
                for candidate in candidates:
                    if (candidate / "home/signal").exists():
                        scope = candidate
                        break
                if scope is None:
                    if time.monotonic() > deadline:
                        raise RuntimeError("tmpfs privacy rendezvous timed out")
                    time.sleep(0.05)
            hosted = [entry for entry in (scope / "tmp").iterdir()]
            self.assertEqual(hosted, [], "tmpfs contents leaked to the host view")
            probed = [entry for entry in (scope / ".limitprobe").iterdir()]
            self.assertEqual(probed, [], "probe contents leaked to the host view")
            self.assertTrue((scope / ".disposable").is_file())
            self.assertEqual(scope.stat().st_mode & 0o777, 0o700)
        finally:
            if owner.poll() is None:
                owner.terminate()
                try:
                    owner.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    owner.kill()
                    owner.wait(timeout=10)
            if scope is not None and scope.exists():
                shutil.rmtree(scope)


if __name__ == "__main__":
    unittest.main()
