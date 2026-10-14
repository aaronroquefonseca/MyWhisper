#!/usr/bin/env bash
# An Xvfb-backed native niri instance isolates clipboard and keyboard traffic.
# Establish isolation before D-Bus activation, not just before GTK startup.
set -euo pipefail
if [[ ${MYWHISPER_NESTED_COMPOSITOR:-} != 1 ]]; then
    repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
    exec python3 "$repo_root/scripts/disposable.py" MYWHISPER_CLIPBOARD_AUDIT_ROOT compositor \
        env MYWHISPER_NESTED_COMPOSITOR=1 xvfb-run -a dbus-run-session -- bash "$0"
fi
root=$MYWHISPER_CLIPBOARD_AUDIT_ROOT
printf 'prefer-no-csd\n' > "$root/config/niri.kdl"
export LIBGL_ALWAYS_SOFTWARE=1
niri --config "$root/config/niri.kdl" > "$root/niri.log" 2>&1 &
compositor=$!
cleanup() { kill "$compositor" 2>/dev/null || true; wait "$compositor" 2>/dev/null || true; }
trap cleanup EXIT
for i in {1..100}; do
    for socket in "$root/runtime"/wayland-*; do
        if [[ -S "$socket" ]]; then
            export WAYLAND_DISPLAY="$socket" GDK_BACKEND=wayland GTK_A11Y=none
            timeout 35 target/debug/examples/clipboard_smoke
            exit 0
        fi
    done
    kill -0 "$compositor" || { cat "$root/niri.log"; exit 1; }
    sleep .05
done
cat "$root/niri.log"
exit 1
