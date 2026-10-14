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

- #11 `fix/server-status-and-switching` (open, ready for review, based
  `092cd80`, head `7c066b16`): touches `src/service.rs`,
  `src/transcribe.rs`, `src/bin/mywhisper.rs`. Overlaps #22 scope
  (R08–R10, R15, R28, R34). This turn changes no code in those files,
  so nothing independently reviewed there is overwritten. #22 must
  preserve #11 behavior on landing; #11 leaving draft does not close
  #22's server/capture acceptance.
- #4 Qwen3-ASR sidecar (open, branch `t3code/be770be5`, head `4c7f3873`): adds
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

README Install/NixOS/System-deps sections aligned with the preserved
reference snapshot (`reference/README.audit-snapshot.md`); code
behavior unchanged, License section preserved:

- System deps split into Build vs Runtime; runtime names libpulse
  (`parec`/`pactl`), Secret Service, input-device permissions and the
  ydotool opt-in. Prior one-line list omitted all of these.
- `parec`/`pactl` verified shipped by Arch `libpulse`
  (`pacman -Ql libpulse`); Nix wrapper already ships `pulseaudio`
  tools. Prior omission finding closed for current manifests.
- Install section claimed `enable --now`; `update.sh` enables and
  restarts the upgraded engine. README now shows enable + restart.
  `--version`/`--help` already precede DB/keyring init
  (`src/bin/mywhisper.rs`).
- NixOS paragraph referenced a per-release `fetchzip` hash; the flake
  builds locked source directly with no binary hash. Replaced with
  source-build wording plus Secret Service deadline, keyring-restore,
  wrapper/nix-ld/CUDA prerequisites and the three VM check commands
  with their explicit limits.

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
  PASS FRESH this turn via the owned-store runner (verified identical
  to the archived #7 runner; store UID 1000/mode 0700/`.disposable`;
  own gate root + own `target/nix-audit-build`; NIX_REMOTE=local in
  bwrap; /dev/kvm bound; no host daemon change):
  `symbolic-logo`, `desktop` (scoped-package VM: System dark, GUI
  close, encrypted storage, repeated user-service start/stop) and
  `whisper-vulkan-loader` (pinned upstream starts via nix-ld as a
  normal VM user) all exit 0 on this branch source, 40 derivations
  including a fresh `mywhisper-0.9.4` source build with its
  `cargoCheckHook` encrypted-fixture suite. Logs:
  /tmp/mw15-nix-logo.log, /tmp/mw15-nix-desktop.log. This proves the
  main-based source only; integrated-tip (#23) Nix acceptance stays
  open until that source is gated.
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

## Sibling draft status (2026-10-14, coordination snapshot)

Superseded by LEDGER.md (fresh tips, verified stack lag 11/2/2).
#11 open and ready for review (`7c066b16`); #4 open (`4c7f3873`).

## Merge-readiness verdict: NOT READY

Blocking, per the handoff's own rule to keep this PR draft until its
scope and required checks are actually complete:

1. #16–#23 code scopes unfinished (#18–#21 strict lint fails from
   layering; #22 new head ungated; #23 CI failure open); final
   acceptance per group needs each owner's regression/live evidence.
2. Running desktop/service upgrade + recovery, GPU inference,
   trusted WSS/paid providers, physical mic, cross-host LAN and
   compositor variants: no evidence, prerequisites only.
3. R49 remaining advertised assets and installer stress open.
4. Integrated-tip (future #23) full gate battery, including Nix,
   still to run; this turn's Nix evidence covers main-based source.
