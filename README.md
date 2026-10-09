# MyWhisper

Local-first voice dictation: press a key, talk, clean text lands anywhere.
GTK4 + Rust, Linux/Wayland-first.

## Pieces

- **Service** (`mywhisper --service`): headless background engine —
  global hotkeys, capture, transcription, sync, heartbeat. Runs as a
  systemd user service (`mywhisper.service`).
- **Pill**: Wayland layer-shell overlay with live mic visualizer,
  streaming transcription preview and an animated Processing state.
- **App** (`mywhisper`): desktop window — Home (health + history),
  Insights (stats + heatmap), Dictionary (words + snippets), Speech
  to Text, Language Models, Sync.
- **Speech to Text**: chained options, top-down fallback. Local
  whisper (HTTP :8080), sherpa online/offline (websocket :6006/:6007),
  Whistle CLI, self-hosted (OpenAI-compat http or sherpa-protocol
  ws/wss, optional bearer token), cloud (OpenAI/Groq/Mistral/Gemini/
  Deepgram). One local server at a time; lazy start with catch-up.
- **Sync**: serverless device-to-device over encrypted TCP
  (Noise_XX, ed25519 session proofs), mDNS discovery, LWW merge.
- **State**: single encrypted `state.db` (`~/.local/share/mywhisper/`),
  SQLCipher with the key in the OS keychain.

## Install (private releases)

Tagged versions ship binary tarballs on the private GitHub Release.

Arch: `packaging/arch/update.sh [version]` downloads the tarball,
builds and installs the package, enables the user service.

NixOS: `packaging/nixos/flake.nix` exposes the package plus
`services.mywhisper.enable` (systemd user service). Needs a GitHub
token in nix.conf (`access-tokens = github.com=...`) and the per-release
`fetchzip` hash (see comments in the flake).

## Build

```bash
cargo fmt && cargo check && cargo build
cargo test
```

## Run (dev)

```bash
./target/debug/mywhisper --service   # background engine
./target/debug/mywhisper             # desktop app
```

## System deps (Arch)

gtk4, gtk4-layer-shell, libadwaita, alsa-lib, pkgconf.
