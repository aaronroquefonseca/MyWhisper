# MyWhisper

Local-first voice dictation: press a key, talk, clean text lands anywhere.
GTK4 + Rust, Linux/Wayland-first.

## Pieces

- **Pill** (`mywhisper` binary): Wayland layer-shell overlay with a live
  mic-level visualizer and an animated, variable-size assistant panel.
- **App** (`mywhisper-app` binary): desktop window with sidebar —
  Home (history), Insights (usage stats + heatmap), Dictionary
  (words + snippets), Settings and Sync (both modal).
- **Sync**: serverless device-to-device over encrypted TCP
  (Noise_XX, ed25519 session proofs), mDNS discovery, LWW merge.
- **State**: single encrypted `state.db` (`~/.local/share/mywhisper/`),
  SQLCipher with the key in the OS keychain.

## Build

```bash
cargo fmt && cargo check && cargo build
cargo test
```

## Run

```bash
./target/debug/mywhisper-app            # desktop app
./target/debug/mywhisper --help         # pill options
```

## System deps (Arch)

gtk4, gtk4-layer-shell, libadwaita, alsa-lib, pkgconf.
