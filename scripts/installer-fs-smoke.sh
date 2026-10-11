#!/usr/bin/env bash
# Only a private mount namespace and a disposable, size-limited tmpfs are filled.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
if [[ ${MYWHISPER_INSTALLFS_CHILD:-} != 1 ]]; then
  exec python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT installfs \
    env MYWHISPER_INSTALLFS_CHILD=1 unshare --user --map-root-user --mount --fork bash "$0"
fi
[[ "$MYWHISPER_AUDIT_ROOT" == /tmp/mywhisper-installfs-audit.* ]]
[[ -f "$MYWHISPER_AUDIT_ROOT/.disposable" ]]
mkdir -m700 "$MYWHISPER_AUDIT_ROOT/fs"
mount --make-rprivate /
mount -t tmpfs -o size=512k,mode=700,nodev,nosuid,noexec tmpfs "$MYWHISPER_AUDIT_ROOT/fs"
trap 'umount "$MYWHISPER_AUDIT_ROOT/fs"' EXIT
timeout 45 "$repo_root/target/debug/examples/installer_fs_probe"
