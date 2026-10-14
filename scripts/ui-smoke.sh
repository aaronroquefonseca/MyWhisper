#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd -- "$repo_root"
cargo build --locked --example ui_preview --example desktop_smoke --example focus_smoke
build_root=${CARGO_TARGET_DIR:-$repo_root/target}
python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT focus16 env MYWHISPER_DB_KEY=0000000000000000000000000000000000000000000000000000000000000000 GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals GSK_RENDERER=cairo dbus-run-session -- xvfb-run -a timeout 90 "$build_root/debug/examples/focus_smoke"
for configuration in 'light en' 'dark es'; do
  read -r theme language <<< "$configuration"
  python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT nativeui env MYWHISPER_DB_KEY=0000000000000000000000000000000000000000000000000000000000000000 GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals GSK_RENDERER=cairo dbus-run-session -- xvfb-run -a timeout 60 "$build_root/debug/examples/ui_preview" home "$theme" "$language" --smoke
  python3 scripts/disposable.py MYWHISPER_AUDIT_ROOT desktopui env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals GSK_RENDERER=cairo dbus-run-session -- xvfb-run -a timeout 15 "$build_root/debug/examples/desktop_smoke" "$theme"
done
