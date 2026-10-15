#!/usr/bin/env bash
# Opt-in real pinned CPU reference download and native trust/verification dialogs.
# Every model, receipt, signing key, database and XDG path is disposable.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd -- "$repo_root"
cargo build --locked --example ui_preview
for configuration in 'light en' 'dark es'; do
    read -r theme language <<< "$configuration"
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT assetsnative \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 90 "$repo_root/target/debug/examples/ui_preview" settings "$theme" "$language" \
        'Speech to Text' --asset-smoke
done
