#!/usr/bin/env bash
# Both native GTK configurations, each with an owned private XDG/D-Bus session.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd -- "$repo_root"
# Build before replacing HOME so the installed Rust toolchain remains usable.
cargo build --locked --example ui_preview
for configuration in 'light en' 'dark es'; do
    read -r theme language <<< "$configuration"
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT ui \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 90 "$repo_root/target/debug/examples/ui_preview" home "$theme" "$language" --smoke
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT modelload \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 15 "$repo_root/target/debug/examples/ui_preview" settings "$theme" "$language" \
        'Language Models' --ollama-load-smoke
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT modeledit \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 15 "$repo_root/target/debug/examples/ui_preview" settings "$theme" "$language" \
        'Language Models' --ollama-load-edit-smoke
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT recovery \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 30 "$repo_root/target/debug/examples/ui_preview" home "$theme" "$language" --settings-recovery
done

# Confirmed reset owns background IO and survives partial cleanup in both themes.
for configuration in 'light en' 'dark es'; do
    read -r theme language <<< "$configuration"
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT reset \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 30 "$repo_root/target/debug/examples/ui_preview" settings "$theme" "$language" System --reset-smoke
    python3 "$repo_root/scripts/disposable.py" MYWHISPER_AUDIT_ROOT resetresume \
        env GDK_BACKEND=x11 GTK_A11Y=none G_DEBUG=fatal-criticals dbus-run-session -- xvfb-run -a \
        timeout 30 "$repo_root/target/debug/examples/ui_preview" settings "$theme" "$language" System --reset-resume-smoke
done
