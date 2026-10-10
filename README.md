# MyWhisper

Local-first voice dictation: press a key, talk, clean text lands anywhere.
GTK4 + Rust, Linux/Wayland-first.

## Pieces

- **Service** (`mywhisper --service`): headless background engine —
  global hotkeys, capture, transcription, sync, heartbeat. Runs as a
  systemd user service (`mywhisper.service`).
- **Pill**: Wayland layer-shell overlay with live mic visualizer,
  streaming transcription preview and an animated Processing state.
- **App** (`mywhisper`): desktop window — Dictation (health + recent
  history), Activity (stats + heatmap), Dictionary (words + spoken
  snippets), Devices, and Settings (including transcription and text cleanup).
- **Speech to Text**: chained options, top-down fallback. Local
  whisper (HTTP :8080), sherpa online/offline (websocket :6006/:6007),
  Whistle CLI, self-hosted (OpenAI-compat http or sherpa-protocol
  ws/wss, optional bearer token), cloud (OpenAI/Groq/Mistral/Gemini/
  Deepgram). One local server at a time; lazy start with catch-up.
- **Sync**: serverless device-to-device over encrypted TCP
  (Noise_XX, ed25519 session proofs), mDNS discovery, LWW merge.
  Compatible updated peers use resumable pages for large histories; older
  peers need an upgrade when their single-message limit is exceeded.
- **State**: single encrypted `state.db` (`~/.local/share/mywhisper/`),
  SQLCipher with the key in the OS keychain.

## Install (private releases)

Tagged versions ship binary tarballs on the private GitHub Release
(`gh` authenticated with an account that can see the repo).

Arch (latest release):

```bash
./packaging/arch/update.sh
```

Specific version:

```bash
./packaging/arch/update.sh 0.9.0
```

The script chooses the highest stable version, verifies the authenticated
GitHub asset digest and archive layout, builds in a disposable directory with
`makepkg -si` (needs sudo), and restarts the upgraded user service:

```bash
systemctl --user enable mywhisper.service
systemctl --user restart mywhisper.service
```

NixOS — flake input (HTTPS + token, no SSH needed):

```nix
mywhisper.url = "github:aaronroquefonseca/MyWhisper?dir=packaging/nixos";
```

SSH alternative:

```nix
mywhisper.url = "git+ssh://git@github.com/aaronroquefonseca/MyWhisper?dir=packaging/nixos";
```

```nix
imports = [ mywhisper.nixosModules.default ];
services.mywhisper.enable = true;
```

Nix builds the authenticated repository source against its pinned Nixpkgs libraries.
Use your existing GitHub access token through Nix's `access-tokens` configuration
or the SSH input above. No separate private tarball or placeholder hash is needed.
The desktop session must provide a running Secret Service (for example GNOME
Keyring or a compatible KWallet service), microphone access, and input-device
permissions. The default login collection must be unlocked before opening MyWhisper;
background processes never request an unlock prompt. Keyring requests have a five-second
deadline. If an encrypted database key is missing, restore its original keyring entry;
MyWhisper preserves the database and never substitutes a new key.
The package provides its default clipboard, paste and PulseAudio tools.
The NixOS module also supplies `nix-ld` and the CPU runtime libraries required by
the downloaded upstream engines. Set `services.mywhisper.enableDownloadedRuntimes = false`
when using only remote providers and supplying your own runtime environment.
CUDA engines additionally require the matching NVIDIA driver and CUDA/cuDNN
libraries; MyWhisper reports missing libraries before starting the selected engine.

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

Preview the UI with disposable sample data:

```bash
cargo run --example ui_preview -- home light en
cargo run --example ui_preview -- settings dark es 'Speech to Text'
cargo run --example ui_preview -- home light en Preferences --smoke
```

See [UI/UX audit, design references, and control behavior](docs/UI_UX.md).

If text has stray marks or missing strokes with the GPU renderer, try
GTK's Cairo renderer for the desktop window:

```bash
GSK_RENDERER=cairo ./target/debug/mywhisper
```

Close the existing desktop window first so the new process takes effect.
This workaround only selects the renderer for that launch; it does not
change system graphics settings or the background service.

## System deps (Arch)

Build: gtk4 >= 4.22, gtk4-layer-shell, libadwaita >= 1.5, alsa-lib,
pkgconf, Rust and Cargo. Runtime: wl-clipboard, wtype, libpulse
(`parec`/`pactl`), and a running Secret Service. The packaged defaults
include the delivery/capture tools; a compatible keyring service remains a
desktop-session requirement. Hotkeys need read permission on input devices;
the optional ydotool paste backend needs ydotool and a running ydotoold.
