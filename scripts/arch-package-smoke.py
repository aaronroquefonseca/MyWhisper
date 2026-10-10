#!/usr/bin/env python3
"""Install a local candidate in disposable rootless Arch userspace.

This checks package files and a same-version pkgrel upgrade, not a running
desktop/user-service upgrade. No host credentials, data or devices are mounted.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


GUEST = r'''
set -euo pipefail
pacman --noconfirm -Syu
pacman --noconfirm -S --needed gtk4 libadwaita gtk4-layer-shell alsa-lib \
  hicolor-icon-theme wl-clipboard wtype libpulse ca-certificates desktop-file-utils
useradd -m builder
install -d -o builder -g builder /work
cp /audit-input/* /work/
chown -R builder:builder /work
cd /work
su builder -c 'cd /work && makepkg --noconfirm'
pacman --noconfirm -U /work/mywhisper-*-1-x86_64.pkg.tar.zst
test "$(pacman -Q mywhisper)" = "mywhisper $CANDIDATE_VERSION-1"
# This user has no session bus/keyring and a fresh empty home. Informational
# CLI flags must not open any store, keyring, audio device or desktop service.
su builder -c 'env -u DBUS_SESSION_BUS_ADDRESS mywhisper --version' > /work/version.txt
test "$(cat /work/version.txt)" = "mywhisper $CANDIDATE_VERSION"
test ! -e /home/builder/.local/share/mywhisper
test ! -e /home/builder/.config/mywhisper
ldd /usr/bin/mywhisper > /work/ldd.txt
cat /work/ldd.txt
! grep -q 'not found' /work/ldd.txt
desktop-file-validate /usr/share/applications/com.mywhisper.App.desktop
systemd-analyze verify /usr/lib/systemd/user/mywhisper.service
test -s /usr/share/licenses/mywhisper/THIRD_PARTY_NOTICES.txt
test -s /usr/share/licenses/mywhisper/THIRD_PARTY.json
test -f /usr/share/icons/hicolor/scalable/apps/com.mywhisper.App.svg
test -f /usr/share/icons/hicolor/symbolic/apps/com.mywhisper.App-symbolic.svg
sed -i 's/^pkgrel=1$/pkgrel=2/' PKGBUILD
su builder -c 'cd /work && makepkg --noconfirm'
pacman --noconfirm -U /work/mywhisper-*-2-x86_64.pkg.tar.zst
test "$(pacman -Q mywhisper)" = "mywhisper $CANDIDATE_VERSION-2"
pacman -Qkk mywhisper
su builder -c 'env -u DBUS_SESSION_BUS_ADDRESS mywhisper --version'
printf 'Arch candidate package install and pkgrel upgrade passed.\n'
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=Path, help="directory containing release.json and its archive")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    metadata = json.loads((args.candidate / "release.json").read_text())
    asset = metadata["asset"]
    if Path(asset).name != asset:
        raise ValueError("candidate asset must have a plain filename")
    archive = args.candidate / asset
    with archive.open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    if digest != metadata["sha256"]:
        raise ValueError("candidate archive checksum differs")
    spec = importlib.util.spec_from_file_location("artifact", repo / "packaging/validate-artifact.py")
    validator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(validator)
    validator.validate(archive, metadata["version"])
    if not metadata["dependency_notices_complete"]:
        raise ValueError("candidate dependency notices incomplete")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", metadata["version"]):
        raise ValueError("Arch installation gate requires a stable candidate")
    pkgbuild = (repo / "packaging/arch/PKGBUILD").read_text()
    pkgbuild, count = re.subn(r"^pkgver=.*$", "pkgver=" + metadata["version"], pkgbuild, flags=re.MULTILINE)
    if count != 1:
        raise ValueError("PKGBUILD needs exactly one version")
    pkgbuild, count = re.subn(r"^sha256sums=.*$", "sha256sums=('" + digest + "')", pkgbuild, flags=re.MULTILINE)
    if count != 1:
        raise ValueError("PKGBUILD needs exactly one checksum")
    # Pin the resolved image by content for this entire check; record it with
    # the candidate digest so subsequent logs identify exactly what ran.
    image = "docker.io/library/archlinux:base-devel"
    subprocess.run(["podman", "pull", "--quiet", image], check=True, timeout=300)
    identifier = subprocess.check_output(["podman", "image", "inspect", "--format", "{{.Id}}", image],
                                         text=True, timeout=15).strip()
    print(json.dumps({"arch_image": identifier, "candidate_sha256": digest,
                      "source_dirty": metadata.get("source_dirty"),
                      "project_license_established": metadata["project_license_established"]}), flush=True)
    with tempfile.TemporaryDirectory(prefix="mywhisper-arch-package-audit.") as directory:
        root = Path(directory)
        root.chmod(0o700)
        (root / ".disposable").touch(mode=0o600)
        inputs = root / "input"
        inputs.mkdir(mode=0o755)
        shutil.copyfile(archive, inputs / asset)
        (inputs / "PKGBUILD").write_text(pkgbuild)
        cid = root / "container-id"
        try:
            subprocess.run([
                "podman", "run", "--rm", "--pull=never", "--interactive",
                "--cidfile", str(cid), "--label", "com.mywhisper.disposable-audit=true",
                "--volume", f"{inputs}:/audit-input:ro",
                "--env", "CANDIDATE_VERSION=" + metadata["version"],
                identifier, "bash", "-s",
            ], input=GUEST, text=True, check=True, timeout=1800)
        finally:
            if cid.exists():
                container = cid.read_text().strip()
                if not re.fullmatch(r"[0-9a-f]{64}", container):
                    raise ValueError("unexpected disposable container identifier")
                # Only the container created by this invocation can be stopped.
                subprocess.run(["podman", "rm", "--force", "--ignore", container],
                               check=True, timeout=30)


if __name__ == "__main__":
    main()
