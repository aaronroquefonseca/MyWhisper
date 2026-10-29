#!/usr/bin/env bash
# Audit probes assert the observed bugs. After fixes, their assertions should fail.
# Run from any directory. All state is disposable; no microphone/cloud calls.
set -euo pipefail
repo_dir=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_dir"
cargo build --locked --lib --example ui_preview
audit_dir=$(mktemp -d /tmp/mywhisper-release-audit.XXXXXX)
export AUDIT_DIR="$audit_dir"
export XDG_DATA_HOME="$audit_dir/xdg/data"
export XDG_CONFIG_HOME="$audit_dir/xdg/config"
export XDG_STATE_HOME="$audit_dir/xdg/state"
export MYWHISPER_DB_KEY=0000000000000000000000000000000000000000000000000000000000000000

# Pick the most recently built libraries; all dependencies come from --locked.
newest_rlib() {
  python3 - "$1" <<'PY'
import glob, os, sys
paths = glob.glob('target/debug/deps/lib' + sys.argv[1] + '-*.rlib')
if not paths:
    raise SystemExit('No library for ' + sys.argv[1])
print(max(paths, key=os.path.getmtime))
PY
}
mywhisper_rlib=$(newest_rlib mywhisper)
serde_rlib=$(newest_rlib serde_json)
gtk_rlib=$(newest_rlib gtk4)
rustc --edition=2024 docs/release-audit/repro.rs -L dependency=target/debug/deps \
  --extern "mywhisper=$mywhisper_rlib" --extern "serde_json=$serde_rlib" \
  -o "$audit_dir/repro"
rustc --edition=2024 docs/release-audit/ui_probe.rs -L dependency=target/debug/deps \
  --extern "mywhisper=$mywhisper_rlib" --extern "gtk4=$gtk_rlib" \
  -o "$audit_dir/ui-probe"

"$audit_dir/repro" > "$audit_dir/repro.log" 2>&1
cat "$audit_dir/repro.log"
for probe in leak lang; do
  xvfb-run -a "$audit_dir/ui-probe" "$probe" > "$audit_dir/ui-$probe.log" 2>&1
  cat "$audit_dir/ui-$probe.log"
done
if xvfb-run -a "$audit_dir/ui-probe" mdns > "$audit_dir/ui-mdns.log" 2>&1; then
  echo 'Discovery probe unexpectedly succeeded; inspect whether the bug is fixed.' >&2
  exit 1
fi
rg 'not a char boundary' "$audit_dir/ui-mdns.log"
echo "Probe logs and disposable data: $audit_dir"
