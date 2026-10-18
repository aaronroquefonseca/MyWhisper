# MyWhisper

Local-first voice dictation: press a key, talk, clean text lands anywhere.
GTK4 + Rust, Linux/Wayland-first.

For fast local PR testing and PR-based release notes, see
[the development workflow](docs/DEVELOPMENT.md).
Release notes use dated, release-linked headings and the categories in [the release configuration](.github/release.yml).

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

The script downloads the tarball, bumps the PKGBUILD, runs
`makepkg -si` (needs sudo), then enables the user service:

```bash
systemctl --user enable --now mywhisper.service
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

Plus a GitHub token in nix.conf for the private tarball
(`access-tokens = github.com=...`) and the per-release `fetchzip`
hash (see comments in `packaging/nixos/flake.nix`).

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

gtk4, gtk4-layer-shell, libadwaita, alsa-lib, pkgconf.

### Packaging audit checks

The Nix flake builds the source against its locked Nixpkgs instead of fetching
an unverified private binary. Its wrapper supplies Adwaita/hicolor fallback
icons, the default paste/capture tools, certificates and the resolver executable.
The NixOS module enables nix-ld and CPU/Vulkan loader libraries for downloaded
upstream engines. Remote-only setups can disable `enableDownloadedRuntimes`.
CUDA inference additionally requires a compatible NVIDIA driver and CUDA/cuDNN
libraries; supplying the Vulkan loader establishes startup, not GPU inference.

Run `nix build ./packaging/nixos#checks.x86_64-linux.desktop`,
`nix build ./packaging/nixos#checks.x86_64-linux.symbolic-logo` and
`nix build ./packaging/nixos#checks.x86_64-linux.whisper-vulkan-loader` for the
scoped clean NixOS package/service, GTK 4.22 SVG and exact downloaded loader
checks. The full release-audit shutdown/privacy/provider requirements remain on
the integration branch.

Candidate archives include authenticated dependency notices and SHA-256 sums.

## License

MyWhisper is licensed under the [MIT License](LICENSE).

Portions adapted from OpenWhispr retain its MIT copyright and license notice in
[packaging/notices/OpenWhispr.txt](packaging/notices/OpenWhispr.txt).
Third-party components retain their own licenses. Release archives and installed
packages include the project license, third-party notices, and their inventory.
