#!/usr/bin/env python3
"""Run a bounded native integration gate with process-owned disposable state."""

import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile

# Kernel-enforced tmpfs scratch bounds for MYWHISPER_AUDIT_TMPFS_TMP=1.
# A plain `--tmpfs` mount has no size limit and mode 0755; these options
# make the bound and permissions real at mount level. Unsupported bwrap
# fails the whole gate (fail closed) since the option is rejected.
TMPFS_SCRATCH_BYTES = 2147483648  # 2 GiB fixture scratch cap
TMPFS_SCRATCH_MODE = "0700"
LIMIT_PROBE_BYTES = 1048576  # 1 MiB ENOSPC enforcement probe


def main(timeout=120):
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
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ]:
            path = root / subdirectory
            path.mkdir(mode=0o700)
            env[variable] = str(path)
        (root / "runtime/keyring").mkdir(mode=0o700)
        temporary = root / "tmp"
        temporary.mkdir(mode=0o700)
        env["TMPDIR"] = str(temporary)
        portal = root / "config/xdg-desktop-portal"
        portal.mkdir(mode=0o700)
        (portal / "portals.conf").write_text("[preferred]\ndefault=none\n")
        env.update(
            GNOME_KEYRING_CONTROL=str(root / "runtime/keyring"),
            GIO_USE_VFS="local",
            GTK_USE_PORTAL="0",
        )
        for variable in (
            "DBUS_SESSION_BUS_ADDRESS",
            "WAYLAND_DISPLAY",
            "WAYLAND_SOCKET",
            "NIRI_SOCKET",
            "MYWHISPER_DB_KEY",
        ):
            env.pop(variable, None)
        # Explicit test-only ephemeral scratch for TMPDIR. The runner root
        # is created on the host filesystem, so TMPDIR-backed fixtures pay
        # whatever the host /tmp costs: tmpfs on dev machines, overlay disk
        # in CI containers (measured ~0.002ms vs ~8.7ms per fsync, ~4000x).
        # MYWHISPER_AUDIT_TMPFS_TMP=1 mounts a private tmpfs on root/tmp
        # with a kernel-enforced 2 GiB cap (--size) and mode 0700 (--perms),
        # owned by the mounting user. Marker, HOME/XDG stubs and evidence
        # ownership are unchanged. Real syscalls are preserved: files,
        # locks, transactions and fsync still execute with identical
        # process-visible semantics (SIGKILL/reopen verified by the suite);
        # only host-reboot persistence differs, which no in-scope test
        # relies on. Filesystem/power-loss/package/upgrade integration
        # gates must NOT use this flag. The variable is popped so the child
        # never inherits it.
        # A 1 MiB probe mount on root/.limitprobe proves kernel size
        # enforcement without allocating gigabytes; its host stub stays
        # visible and empty while the child fills the overlaid mount.
        tmpfs_scratch = env.pop("MYWHISPER_AUDIT_TMPFS_TMP", "")
        if tmpfs_scratch not in ("", "1"):
            raise SystemExit("MYWHISPER_AUDIT_TMPFS_TMP must be unset or 1")
        limit_probe = root / ".limitprobe"
        if tmpfs_scratch == "1":
            limit_probe.mkdir(mode=0o700)
        if shutil.which("bwrap") is None:
            raise SystemExit("bubblewrap is required for disposable process ownership")
        # A process group cannot contain setsid()/timeout/daemon descendants.
        # The kernel's PID namespace owns every descendant, even if this runner
        # is SIGKILLed. Its private /tmp also prevents namespace-local Xvfb PIDs
        # from being mistaken for stale host display locks or socket owners.
        owned_command = [
            "bwrap",
            "--bind",
            "/",
            "/",
            "--unshare-user",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--dev-bind",
            "/dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--bind",
            str(root),
            str(root),
        ]
        if tmpfs_scratch == "1":
            # --perms/--size apply only to the immediately following --tmpfs;
            # unknown options make bwrap itself fail, failing the gate closed.
            owned_command.extend(
                [
                    "--perms",
                    TMPFS_SCRATCH_MODE,
                    "--size",
                    str(TMPFS_SCRATCH_BYTES),
                    "--tmpfs",
                    str(temporary),
                    "--perms",
                    TMPFS_SCRATCH_MODE,
                    "--size",
                    str(LIMIT_PROBE_BYTES),
                    "--tmpfs",
                    str(limit_probe),
                ]
            )
        owned_command.extend(["--die-with-parent", "--new-session"])
        # Explicit external /tmp inputs, such as the private Nix store or an
        # acceptance-test rendezvous socket, remain readable. All gate outputs
        # and temporary files belong in the writable, marked disposable root.
        inputs = {
            Path(argument)
            for argument in command
            if argument.startswith("/tmp/")
            and Path(argument).exists()
            and not Path(argument).is_relative_to(root)
        }
        # Library/resolver/certificate fixtures can be supplied through these
        # standard variables instead of positional arguments. Preserve their
        # explicit inputs without making the host's whole /tmp visible.
        for variable in (
            "PATH",
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "ALSA_CONFIG_PATH",
        ):
            for item in env.get(variable, "").split(os.pathsep):
                path = Path(item)
                if (
                    item.startswith("/tmp/")
                    and path.exists()
                    and not path.is_relative_to(root)
                ):
                    inputs.add(path)
        for path in sorted(inputs, key=lambda path: (len(path.parts), str(path))):
            owned_command.extend(["--ro-bind", str(path), str(path)])
        owned_command.extend(["--", *command])
        process = subprocess.Popen(owned_command, env=env, start_new_session=True)
        try:
            return process.wait(timeout=timeout)
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
