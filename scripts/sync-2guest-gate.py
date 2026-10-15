#!/usr/bin/env python3
"""Two-peer real pairing/discovery/sync gate driver (sync scope).

All fixture writes (trust rows, settings blob, synthetic history,
revocation rows) happen WHILE THE APP IS STOPPED, before first or next
boot; every runtime behavior observed is genuine engine execution
(startup application, mDNS discovery, Noise dials, authz, tombstones,
resume, denial). Direct SQL mutation is NEVER used mid-run to fake a
transition.

ABSENT control paths on this branch (GUI-only downstream — NOT covered
here, NOT relabeled as covered):
  A1 pairing acceptance click / live revocation / untrust (no peers UI;
      upsert/remove_peer reachable only via lib/tests).
  A2 mid-run sync toggle / settings edits (no sync UI; save_settings
      merge path is unit-covered).
  A3 live reset during sync (danger-zone reset is GUI-only; reset worker
      is #22/#23-owned).
  A4 history deletion during sync (clear-all UI only; tombstone receive
      path is unit-covered).
Covered instead, honestly: pre-seeded states + REAL startup application;
REAL discovery (mDNS required); REAL untrusted-dial rejection with
pending row; REAL transport/tombstones/resume/revocation-denial; REAL
SIGKILL crash recovery. Trust outcome seeding stands in for the A1
click; everything after it is engine truth.

Backends: --backend local (single-engine selftest only; loopback has no
multicast here, so peer gates SKIP with reason) or --backend ssh
--guest-a USER@HOST --guest-b USER@HOST (full gates; sqlcipher CLI on
guests for encrypted-DB probes; python3 NOT required on guests).
Guest plan: --guest-spec (packages/network/env for #15's Nix
translation — this file owns gate logic only, never VM provisioning).

Usage:
  python3 scripts/sync-2guest-gate.py --backend local --bin ./target/debug/mywhisper
  python3 scripts/sync-2guest-gate.py --backend ssh --guest-a root@192.168.77.2 \\
      --guest-b root@192.168.77.3 --bin /run/current-system/sw/bin/mywhisper
  python3 scripts/sync-2guest-gate.py --guest-spec
"""

import argparse
import json
import os
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time

GUEST_SPEC = """
# Guest spec for #15's Nix translation (one per guest, identical shape).
packages: mywhisper (final packaged bin), cage OR xvfb (display for
  --service GTK thread; pill layer-shell prefers cage — verify non-fatal
  at implementation, report), sqlcipher (encrypted-DB probes), openssh,
  jq, sqlite3.
network: libvirt isolated net, static DHCP (.2/.3), NO LAN forward;
  bridge multicast ON for 224.0.0.251:5353 (app uses built-in mdns-sd,
  no avahi daemon needed; verify guest-to-guest UDP 5353 at bringup).
env per guest (0600 provisioning, never shared): HOME fresh,
  MYWHISPER_DB_KEY=<distinct 64hex> (production-supported, no keyring),
  XDG_* under HOME, no audio input, STT warmup disabled via pre-seeded
  settings (no model downloads).
pre-seed (BEFORE first boot, fixture-labeled): settings blob
  (hotkeys off, stt.auto_start_server=false, sync on).
probes: sqlcipher CLI on guest DBs, journald/app logs, virsh console+SSH.
RAM: 1.2 GB/guest cap suggested (service is light; STT/models off).
"""

DISCOVER_TIMEOUT = 90
CONVERGE_TIMEOUT = 120


class Fail(Exception):
    pass


def check(condition, message, evidence=None):
    if not condition:
        detail = "" if evidence is None else f" :: {evidence}"
        raise Fail(f"{message}{detail}")


class Guest:
    """One engine instance (subprocess locally, ssh on guests)."""

    def __init__(self, name, home, key, binary, ssh=None):
        self.name = name
        self.home = home
        self.key = key
        self.binary = binary
        self.ssh = ssh
        self.proc = None
        self.remote_pid = None
        self.log_path = os.path.join(home, f"{name}.log")

    def base_env(self):
        return {
            "HOME": self.home,
            "XDG_CONFIG_HOME": self.home + "/.config",
            "XDG_DATA_HOME": self.home + "/.local/share",
            "XDG_STATE_HOME": self.home + "/.local/state",
            "XDG_CACHE_HOME": self.home + "/.cache",
            "XDG_RUNTIME_DIR": self.home + "/.run",
            "MYWHISPER_DB_KEY": self.key,
        }

    def start(self):
        self.stop(kill=True)
        if self.ssh is None:
            for sub in (
                "",
                "/.config",
                "/.local/share",
                "/.local/state",
                "/.cache",
                "/.run",
            ):
                os.makedirs(self.home + sub, mode=0o700, exist_ok=True)
            log = open(self.log_path, "ab")
            env = dict(os.environ)
            env.pop("DBUS_SESSION_BUS_ADDRESS", None)
            env.update(self.base_env())
            argv = [self.binary, "--service"]
            if not env.get("DISPLAY") and not env.get("WAYLAND_DISPLAY"):
                if shutil.which("xvfb-run") is None:
                    raise Fail(f"{self.name}: no display and no xvfb-run")
                argv = ["xvfb-run", "-a"] + argv
            self.proc = subprocess.Popen(
                argv, env=env, stdout=log, stderr=subprocess.STDOUT
            )
        else:
            remote_env = " ".join(f"{k}='{v}'" for k, v in self.base_env().items())
            setup = (
                f"mkdir -p -m 700 {self.home}/.config "
                f"{self.home}/.local/share {self.home}/.local/state "
                f"{self.home}/.cache {self.home}/.run; "
                f"{remote_env} nohup {self.binary} --service "
                f">{self.log_path} 2>&1 & echo $!"
            )
            result = subprocess.run(
                ["ssh", self.ssh, setup], capture_output=True, text=True
            )
            pid = result.stdout.strip()
            check(
                pid.isdigit(), f"{self.name}: remote start failed", result.stderr[:200]
            )
            self.remote_pid = pid
        self.wait_log(r"\[sync\] (listening|applying engine)", 30)

    def stop(self, kill=False):
        flag = "KILL" if kill else "TERM"
        if self.ssh is None:
            if self.proc is not None and self.proc.poll() is None:
                try:
                    self.proc.send_signal(signal.SIGKILL if kill else signal.SIGTERM)
                    self.proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait()
            self.proc = None
        elif self.remote_pid:
            subprocess.run(
                ["ssh", self.ssh, f"kill -{flag} {self.remote_pid} 2>/dev/null; true"]
            )
            self.remote_pid = None

    def read_log(self):
        if self.ssh is None:
            with open(self.log_path, "rb") as handle:
                return handle.read().decode("utf-8", "replace")
        result = subprocess.run(
            ["ssh", self.ssh, f"cat {self.log_path}"], capture_output=True, text=True
        )
        return result.stdout

    def wait_log(self, pattern, timeout):
        deadline = time.time() + timeout
        compiled = re.compile(pattern)
        while time.time() < deadline:
            if compiled.search(self.read_log()):
                return
            time.sleep(1)
        raise Fail(f"{self.name}: log pattern never appeared: {pattern}")

    def find_db(self):
        if self.ssh is None:
            hits = [
                os.path.join(root_dir, f)
                for root_dir, _, files in os.walk(self.home)
                for f in files
                if f == "state.db"
            ]
        else:
            result = subprocess.run(
                ["ssh", self.ssh, f"find {self.home} -name state.db"],
                capture_output=True,
                text=True,
            )
            hits = result.stdout.strip().split()
        check(len(hits) == 1, f"{self.name}: expected one state.db", str(hits))
        return hits[0]

    def query(self, sql):
        """Read the encrypted store via the sqlcipher CLI (actual tooling).

        stdlib sqlite3 cannot read SQLCipher databases; that limitation
        is never silently bypassed (fail loudly if sqlcipher is absent).
        """
        pragma = f"PRAGMA key=\"x'{self.key}'\"; {sql}"
        if self.ssh is None:
            if shutil.which("sqlcipher") is None:
                raise Fail(
                    "local backend needs the sqlcipher CLI for "
                    "encrypted-DB probes (absent here; guests carry "
                    "it per spec)"
                )
            result = subprocess.run(
                ["sqlcipher", self.find_db(), pragma], capture_output=True, text=True
            )
        else:
            result = subprocess.run(
                ["ssh", self.ssh, f"sqlcipher '{self.find_db()}' '{pragma}'"],
                capture_output=True,
                text=True,
            )
        check(
            result.returncode == 0,
            f"{self.name}: encrypted-DB probe failed",
            result.stderr[:300],
        )
        return result.stdout


def seed_settings_blob(guest, mutate):
    """FIXTURE (stopped app only): rewrite kv.settings JSON via SQLCipher.

    The engine applies it at next startup through the real path
    (watcher + start_sync_engine). Shape-guarded: unknown schema keys
    refuse the write instead of corrupting the blob.
    """
    raw = guest.query("SELECT value FROM kv WHERE key='settings';").strip()
    check(raw.startswith("{"), f"{guest.name}: no settings blob yet", raw[:80])
    blob = json.loads(raw)
    check(
        set(blob) >= {"sync", "hotkeys", "stt", "privacy", "preferences"},
        f"{guest.name}: settings schema drift, refusing blind write",
        str(sorted(blob)),
    )
    mutate(blob)
    updated = json.dumps(blob, separators=(",", ":")).replace("'", "''")
    guest.query(f"UPDATE kv SET value='{updated}' WHERE key='settings';")


def seed_trust(guest, peer_node_id, peer_name):
    """FIXTURE (stopped app only): trust OUTCOME without the accept click.

    The pairing-acceptance click itself (A1) has no headless trigger;
    this records its outcome so transport/auth legs run for real.
    """
    guest.query(
        "INSERT INTO devices(node_id,name,trusted,pending) "
        f"VALUES('{peer_node_id}','{peer_name}',1,0) "
        "ON CONFLICT(node_id) DO UPDATE SET trusted=1, pending=0;"
    )


def seed_history(guest, uuid, text):
    """FIXTURE (stopped app only): one history row; table INSERT triggers
    maintain the sync ledger exactly as app writes do."""
    safe = text.replace("'", "''")
    guest.query(
        "INSERT INTO history(uuid,text,raw_text,words,duration_secs) "
        f"VALUES('{uuid}','{safe}','{safe}',3,1);"
    )


def pending_peer_id(guest):
    """Node id of the peer this guest recorded as pending (post-dial)."""
    rows = guest.query("SELECT node_id FROM devices WHERE pending=1 AND trusted=0;")
    ids = [line.strip() for line in rows.strip().splitlines() if line.strip()]
    check(len(ids) == 1, f"{guest.name}: expected one pending peer", str(ids))
    return ids[0]


def wait_converge(guest, uuid, text, timeout=CONVERGE_TIMEOUT):
    deadline = time.time() + timeout
    while time.time() < deadline:
        got = guest.query(
            f"SELECT text FROM history WHERE uuid='{uuid}' AND deleted=0;"
        ).strip()
        if got == text:
            return
        time.sleep(2)
    raise Fail(f"{guest.name}: row never converged", uuid)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--backend", choices=("local", "ssh"), default="")
    parser.add_argument("--guest-a", default="")
    parser.add_argument("--guest-b", default="")
    parser.add_argument("--bin", default="")
    parser.add_argument("--guest-spec", action="store_true")
    parser.add_argument(
        "--gate",
        default="all",
        choices=(
            "all",
            "selftest",
            "discovery",
            "converge",
            "disable",
            "cancel",
            "revoke",
        ),
    )
    args = parser.parse_args()
    if args.guest_spec:
        print(GUEST_SPEC)
        return 0
    check(args.backend in ("local", "ssh"), "--backend is required")
    check(args.bin, "--bin is required")
    stop_after = args.gate if args.gate != "all" else "revoke"
    work = tempfile.mkdtemp(prefix="sync-2guest-gate.")
    guests = []
    try:
        if args.backend == "local":
            guests = [
                Guest("a", work + "/a-home", secrets.token_hex(32), args.bin),
                Guest("b", work + "/b-home", secrets.token_hex(32), args.bin),
            ]
            multicast = False
        else:
            check(args.guest_a and args.guest_b, "ssh needs both guests")
            guests = [
                Guest(
                    "a",
                    "/var/lib/mywhisper-gate",
                    secrets.token_hex(32),
                    args.bin,
                    ssh=args.guest_a,
                ),
                Guest(
                    "b",
                    "/var/lib/mywhisper-gate",
                    secrets.token_hex(32),
                    args.bin,
                    ssh=args.guest_b,
                ),
            ]
            multicast = True
        a, b = guests
        # G0 selftest: boot, listener, mDNS daemon, clean kill/restart.
        for guest in guests:
            guest.start()
        for guest in guests:
            guest.stop()
            guest.start()
        if stop_after == "selftest":
            return 0
        check(
            multicast,
            "peer gates need the multicast bridge backend "
            "(local run covers selftest only)",
        )
        # Boot both fresh; startup catch-up dials discovered peers.
        # G1 discovery + G2 untrusted denial: pending rows prove mDNS
        # discovery, Noise dial, authz denial and contact recording.
        seed_history(a, "synthetic-queued-1", "synthetic queued one")
        seed_history(b, "synthetic-queued-2", "synthetic queued two")
        for guest in guests:
            guest.stop()
        for guest in guests:
            guest.start()
        deadline = time.time() + DISCOVER_TIMEOUT
        while True:
            try:
                a_peer = pending_peer_id(a)
                b_peer = pending_peer_id(b)
                break
            except Fail:
                check(
                    time.time() < deadline, "mutual discovery/pending never converged"
                )
                time.sleep(2)
        for uuid in ("synthetic-queued-1", "synthetic-queued-2"):
            owner, outsider = (a, b) if uuid == "synthetic-queued-1" else (b, a)
            check(
                owner.query(
                    f"SELECT count(*) FROM history WHERE uuid='{uuid}' AND deleted=0;"
                ).strip()
                == "1",
                f"{owner.name}: seeded row missing",
                uuid,
            )
            check(
                outsider.query(
                    f"SELECT count(*) FROM history WHERE uuid='{uuid}' AND deleted=0;"
                ).strip()
                == "0",
                f"{outsider.name}: untrusted content leaked",
                uuid,
            )
        if stop_after == "discovery":
            return 0
        # G3 trust outcome seeded pre-restart (FIXTURE, see A1); restart
        # applies it; the discovery watcher auto-dials appeared trusted
        # peers and transport converges for real.
        for guest in guests:
            guest.stop()
        seed_trust(a, b_peer, "synthetic b")
        seed_trust(b, a_peer, "synthetic a")
        for guest in guests:
            guest.start()
        wait_converge(b, "synthetic-queued-1", "synthetic queued one")
        wait_converge(a, "synthetic-queued-2", "synthetic queued two")
        if stop_after == "converge":
            return 0
        # G4 disable-via-restart (REAL startup application; mid-run
        # toggle has no headless trigger, see A2).
        for guest in guests:
            guest.stop()
        seed_settings_blob(a, lambda blob: blob["sync"].update(enabled=False))
        seed_history(a, "synthetic-quiet-1", "synthetic quiet one")
        for guest in guests:
            guest.start()
        time.sleep(12)
        rows = b.query(
            "SELECT count(*) FROM history WHERE uuid='synthetic-quiet-1';"
        ).strip()
        check(rows == "0", f"{b.name}: export while disabled", rows)
        for guest in guests:
            guest.stop()
        seed_settings_blob(a, lambda blob: blob["sync"].update(enabled=True))
        for guest in guests:
            guest.start()
        wait_converge(b, "synthetic-quiet-1", "synthetic quiet one")
        if stop_after == "disable":
            return 0
        # G6 active cancellation: bulk row staged pre-start, kill -9 the
        # receiver mid-transfer, REAL resume must converge with no
        # partial publish. Bulk exceeds the 16 MiB segmentation floor so
        # the transfer spans many rounds (a small row would finish
        # before the kill lands).
        for guest in guests:
            guest.stop()
        seed_history(a, "synthetic-bulk-1", "synthetic-bulk-data." * 800000)
        for guest in guests:
            guest.start()
        time.sleep(4)
        b.stop(kill=True)
        b.start()
        wait_converge(
            b, "synthetic-bulk-1", "synthetic-bulk-data." * 800000, timeout=180
        )
        if stop_after == "cancel":
            return 0
        # G7 revoke-via-restart (REAL startup denial; live revocation
        # click has no headless trigger, see A1).
        for guest in guests:
            guest.stop()
        b.query(
            "INSERT INTO revoked_devices(node_id) "
            f"VALUES('{a_peer}') "
            "ON CONFLICT(node_id) DO NOTHING;"
        )
        seed_history(a, "synthetic-denied-1", "synthetic denied one")
        for guest in guests:
            guest.start()
        time.sleep(15)
        rows = b.query(
            "SELECT count(*) FROM history WHERE uuid='synthetic-denied-1';"
        ).strip()
        check(rows == "0", f"{b.name}: revoked content leaked", rows)
        log = b.read_log()
        lowered = log.lower()
        check(
            "revok" in lowered or "denied" in lowered or "not trusted" in lowered,
            f"{b.name}: no revocation denial evidence in logs",
        )
        return 0
    finally:
        try:
            for guest in guests:
                guest.stop(kill=True)
        finally:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Fail as error:
        print(f"GATE FAILURE: {error}", file=sys.stderr)
        sys.exit(1)
