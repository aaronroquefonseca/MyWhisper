# PR #15 acceptance record — R01–R51 coordination, R48/R51 focus

Branch: `docs/audit-work-handoffs`. Base: `main` at `092cd80`.
This entry covers work through `4cb536c` plus the README R48
corrections and gate evidence below. PR stays draft. No merge, tag,
release or deployment.

## Ownership

R01–R51 matrix in FINDINGS.md unchanged this turn. #15 owns R06
(first-run identity atomicity), R48 (packaging/prerequisites) and
R51 (release automation/artifact integrity) coordination plus final
acceptance across all groups. #16/#17 standalone; #18→#19→#20→#21→
#22→#23 coupled core. No ownership moves; no closures asserted.

## Overlap reconciliation (no overwrites)

- #11 `fix/server-status-and-switching` (OPEN, based `092cd80`):
  touches `src/service.rs`, `src/transcribe.rs`, `src/bin/mywhisper.rs`.
  Overlaps #22 scope (R08–R10, R15, R28, R34). This turn changes no
  code in those files, so nothing independently reviewed there is
  overwritten. #22 must preserve #11 behavior on landing.
- #4 Qwen3-ASR sidecar (OPEN, branch `t3code/be770be5`): adds
  `src/qwen_runtime.rs`, `src/qwen_server.py`, extends `stt.rs`/
  `transcribe.rs`/settings. Main has no Qwen files. No draft in the
  #15–#23 set may add competing Qwen files or claim that feature.
- Historical probes assert old bugs by design; they are evidence
  only and never the passing regression suite.

## Storage/privacy/cleanup review (state recorded, fixes owned elsewhere)

- WAL journal mode with checkpoint-on-clean-shutdown (`src/store.rs`).
- Schema `user_version = 2`; plaintext→encrypted migration runs every
  load; identity preserved, malformed identities kept as errors.
- File logging bounded (`LOG_KEEP = 5`, `src/paths.rs`).
- Audio retention behind `keep_audio_recordings` (default false) with
  retention-days maintenance. Engine-divergent retention and
  delivery-only maintenance (R39–R41) remain owned by #18/#22.
- Private staging/atomic install/locks for downloads (R49) owned by
  #20/#22; this turn did not re-verify them beyond the gates below.

## R48 doc corrections (this turn)

README fixes, code behavior unchanged:

- System deps list was `gtk4, gtk4-layer-shell, libadwaita,
  alsa-lib, pkgconf`. Now matches PKGBUILD: gtk4 ≥ 4.22,
  gtk4-layer-shell, libadwaita ≥ 1.5, alsa-lib, hicolor-icon-theme,
  wl-clipboard, wtype, libpulse, ca-certificates, pkgconf, plus
  gnome-keyring/ydotool optdepends.
- `parec`/`pactl` verified shipped by Arch `libpulse`
  (`pacman -Ql libpulse`); Nix wrapper already ships `pulseaudio`
  tools. Prior omission finding closed for current manifests.
- Install section claimed `enable --now`; `update.sh` enables and
  restarts the upgraded engine. README now says restart + status
  check. `--version`/`--help` already precede DB/keyring init
  (`src/bin/mywhisper.rs`).
- NixOS paragraph referenced a per-release `fetchzip` hash; the flake
  builds locked source directly with no binary hash. Corrected.

## Gates executed (this turn, current source)

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --locked --all-targets -- -D warnings`: pass.
- `cargo test --locked --all-targets`: 109 passed, 0 failed,
  5 ignored (unchanged opt-in runtime tests).
- `packaging/test-release.py`: 11 passed. `test-upgrade.py`:
  5 passed. `test-cli.py` (no-keyring/no-data CLI): 2 passed.
- `packaging/release.py v0.9.4` preflight: pass, notes resolve.
- `cargo audit --deny warnings` (cargo-audit 0.22.2, 470 locked
  crates, 1310 advisories): pass, zero vulnerabilities/warnings.
- `scripts/ui-smoke.sh` (xvfb, both light-en and dark-es): pass,
  including toast palette/close-hover/dismiss regression. Portal/fuse
  warnings are container noise, non-fatal.
- Candidate `release.py v0.9.4 --output`: clean
  (`source_dirty: false`, notices complete, license established),
  asset `mywhisper-v0.9.4-x86_64-unknown-linux-gnu.tar.gz`,
  sha256 `8d7f5448…1d67` (full value in release.json at run time).
- `validate-artifact.py` + `SHA256SUMS` check: pass.
- `scripts/arch-package-smoke.py` (rootless Podman, clean Arch
  userspace): install + pkgrel upgrade pass — versioned binary,
  no store/keyring/data created by `--version`, ELF deps resolved,
  desktop/service files validate, package integrity clean.

## Not run — exact prerequisites (no success assumed)

- Nix source build + NixOS module/desktop/loader/logo checks:
  `nix build ./packaging/nixos#checks.x86_64-linux.{desktop,
  symbolic-logo,whisper-vulkan-loader}`. Blocked here: nix 2.35.3
  present but `/nix/var/nix/db` permission denied (no daemon
  access). Needs Nix with daemon/disposable-store rights; VM checks
  additionally need KVM + network for locked nixpkgs fetch.
- Running Arch desktop/service upgrade and recovery: booted Arch
  desktop with user systemd, real service instance, GUI session.
- Downloaded-runtime GPU inference: NVIDIA driver + CUDA/cuDNN
  libraries, network to upstream runtime/model endpoints, GPU host.
- Trusted authenticated WSS, paid provider credentials, physical
  microphone/device-change, cross-host LAN pairing/revocation,
  non-niri compositor variants (Sway/GNOME/KDE): no credentials or
  hardware configured in this environment; fixtures do not substitute.
- Full R49 remaining-asset downloads and installer stress: explicit
  follow-up per FINDINGS.md owners.
