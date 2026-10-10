#!/usr/bin/env python3
"""Run a bounded native integration gate with process-owned disposable state."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile


def main():
    if len(sys.argv) < 4:
        raise SystemExit("usage: disposable.py ROOT_ENV LABEL COMMAND [ARGS...]")
    root_variable, label, *command = sys.argv[1:]
    if not root_variable.startswith("MYWHISPER_") or not label.isalnum():
        raise SystemExit("invalid disposable gate identifier")
    with tempfile.TemporaryDirectory(prefix=f"mywhisper-{label}-audit.") as directory:
        root = Path(directory)
        root.chmod(0o700)
        (root / ".disposable").touch(mode=0o600)
        env = os.environ.copy()
        env[root_variable] = str(root)
        for variable, subdirectory in [
            ("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"), ("XDG_RUNTIME_DIR", "runtime"),
        ]:
            path = root / subdirectory
            path.mkdir(mode=0o700)
            env[variable] = str(path)
        (root / "runtime/keyring").mkdir(mode=0o700)
        portal = root / "config/xdg-desktop-portal"
        portal.mkdir(mode=0o700)
        (portal / "portals.conf").write_text("[preferred]\ndefault=none\n")
        env.update(GNOME_KEYRING_CONTROL=str(root / "runtime/keyring"),
                   GIO_USE_VFS="local", GTK_USE_PORTAL="0")
        for variable in ("DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY",
                         "WAYLAND_SOCKET", "NIRI_SOCKET", "MYWHISPER_DB_KEY"):
            env.pop(variable, None)
        process = subprocess.Popen(command, env=env, start_new_session=True)
        try:
            return process.wait(timeout=120)
        finally:
            # Only this runner's process group can be signalled. Keep its
            # leader unreaped until shutdown when it is still running.
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


if __name__ == "__main__":
    sys.exit(main())
