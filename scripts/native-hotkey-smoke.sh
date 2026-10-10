#!/usr/bin/env bash
# Required native companion to the deterministic descriptor/event unit checks.
# Each uinput test grabs only its own device before emitting any key event.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd -- "$repo_root"
# Compile with the installed toolchain before replacing HOME/XDG state. Run
# the resulting test executable directly inside the disposable environment.
probe_binary=$(cargo test --locked --lib --no-run --message-format=json | python3 -c '
import json,sys
executables=[]
for line in sys.stdin:
    item=json.loads(line)
    if item.get("reason")=="compiler-artifact" and item.get("profile",{}).get("test") and item.get("executable"):
        executables.append(item["executable"])
if len(executables)!=1: sys.exit("Expected exactly one library test executable")
print(executables[0])')
exec python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT hotkey \
    env MYWHISPER_NATIVE_HOTKEY_GATE=1 "$probe_binary" hotkey::tests --nocapture
