#!/usr/bin/env bash
# A real disposable Secret Service and independent processes, never host secrets.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
if [[ ${MYWHISPER_KEYRING_CHILD:-} != 1 ]]; then
  exec python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT keyring \
    env MYWHISPER_KEYRING_CHILD=1 dbus-run-session -- bash "$0"
fi
unset MYWHISPER_DB_KEY
export XDG_CONFIG_HOME="$MYWHISPER_AUDIT_ROOT/config" XDG_DATA_HOME="$MYWHISPER_AUDIT_ROOT/data"
export XDG_STATE_HOME="$MYWHISPER_AUDIT_ROOT/state" XDG_RUNTIME_DIR="$MYWHISPER_AUDIT_ROOT/runtime"
export XDG_CACHE_HOME="$MYWHISPER_AUDIT_ROOT/cache" GNOME_KEYRING_CONTROL="$MYWHISPER_AUDIT_ROOT/runtime/keyring"
mkdir -m700 -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME" "$GNOME_KEYRING_CONTROL"
printf '%s' 'disposable-audit-password' | gnome-keyring-daemon --foreground --unlock --components=secrets --control-directory "$GNOME_KEYRING_CONTROL" > "$MYWHISPER_AUDIT_ROOT/daemon.log" 2>&1 &
daemon=$!
trap 'kill -CONT "$daemon" 2>/dev/null || true; kill "$daemon" 2>/dev/null || true; wait "$daemon" 2>/dev/null || true' EXIT
ready=false
for _ in {1..100}; do
  if gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.freedesktop.secrets | rg -q true; then
    ready=true
    break
  fi
  kill -0 "$daemon"
  sleep .05
done
[[ $ready == true ]] || { echo 'Disposable Secret Service did not become ready' >&2; exit 1; }
workers=()
for index in {1..8}; do
  timeout 20 "$repo_root/target/debug/examples/store_probe" add "$index" &
  workers+=("$!")
done
for worker in "${workers[@]}"; do wait "$worker"; done
timeout 20 "$repo_root/target/debug/examples/store_probe" verify 8
timeout 20 "$repo_root/target/debug/examples/store_probe" missing-key-recovery
timeout 20 "$repo_root/target/debug/examples/store_probe" verify 8
printf 'Disposable real Secret Service: eight concurrent processes, shared identity/key, missing-key refusal and recovery passed.\n'
timeout 20 "$repo_root/target/debug/examples/store_probe" duplicate-key-recovery
timeout 20 "$repo_root/target/debug/examples/store_probe" locked-keyring
# Restart this private daemon to exercise fresh-process login unlocking and
# persisted key recovery. --unlock on an initialized daemon does not reliably
# unlock a collection explicitly locked through the Secret Service API.
kill "$daemon"
wait "$daemon" || true
printf '%s' 'disposable-audit-password' | gnome-keyring-daemon --foreground --unlock --components=secrets --control-directory "$GNOME_KEYRING_CONTROL" > "$MYWHISPER_AUDIT_ROOT/restarted-daemon.log" 2>&1 &
daemon=$!
ready=false
for _ in {1..100}; do
  if gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.freedesktop.secrets | rg -q true; then
    ready=true
    break
  fi
  kill -0 "$daemon"
  sleep .05
done
[[ $ready == true ]] || { echo 'Restarted disposable Secret Service did not become ready' >&2; exit 1; }
timeout 20 "$repo_root/target/debug/examples/store_probe" verify 8
# Pause only the daemon PID created above, never a host keyring. The EXIT
# trap resumes it before terminating it, including if this probe fails.
kill -STOP "$daemon"
timeout 20 "$repo_root/target/debug/examples/store_probe" stalled-keyring
kill -CONT "$daemon"
timeout 20 "$repo_root/target/debug/examples/store_probe" verify 8
timeout 45 "$repo_root/target/debug/examples/store_probe" sync-lifecycle
