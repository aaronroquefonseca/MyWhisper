#!/usr/bin/env bash
# Real CPAL backend, all supported sample formats and mono/stereo/6-channel.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd -- "$repo_root"
probe_binary=$(cargo test --locked --lib --no-run --message-format=json | python3 -c '
import json,sys
executables=[]
for line in sys.stdin:
    item=json.loads(line)
    if item.get("reason")=="compiler-artifact" and item.get("profile",{}).get("test") and item.get("executable"):
        executables.append(item["executable"])
if len(executables)!=1: sys.exit("Expected exactly one library test executable")
print(executables[0])')
for format in f32 i16 u16; do
    for channels in 1 2 6; do
        python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT cpal \
            python3 "$repo_root/scripts/cpal-smoke.py" "$probe_binary" "$format" "$channels"
    done
done
