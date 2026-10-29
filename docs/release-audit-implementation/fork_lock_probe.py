"""Demonstrate inherited flock lifetime using only a disposable directory."""
import fcntl
import os
from pathlib import Path
import tempfile

root = Path(tempfile.mkdtemp(prefix='mywhisper-fork-lock-proof-'))
(root / '.disposable').touch(mode=0o600)
lock = os.open(root / 'capture.lock', os.O_CREAT | os.O_RDWR | os.O_CLOEXEC, 0o600)
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
ready_r, ready_w = os.pipe()
release_r, release_w = os.pipe()
child = os.fork()
if child == 0:
    os.close(ready_r)
    os.close(release_w)
    os.write(ready_w, b'forked')
    os.read(release_r, 1)
    os._exit(0)
os.close(ready_w)
os.close(release_r)
try:
    assert os.read(ready_r, 6) == b'forked'
    os.close(lock)
    probe = os.open(root / 'capture.lock', os.O_RDWR | os.O_CLOEXEC)
    try:
        fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        raise AssertionError('inherited open description should still own flock')
    except BlockingIOError:
        pass
finally:
    os.write(release_w, b'x')
    os.close(release_w)
    assert os.waitpid(child, 0)[1] == 0
fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
os.close(probe)
os.close(ready_r)
print('O_CLOEXEC flock remains owned after parent close until inherited child exits; ownership ends after reaping.')
