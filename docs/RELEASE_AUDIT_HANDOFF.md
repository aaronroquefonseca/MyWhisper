# Release-audit testing handoff

Branch: **`fix/release-audit`**, based on immutable audit commit
`f018ab855af41759e275c80708d67973f2890587`.
The historical branch `t3code/pre-release-audit` remains unchanged.

This is unfinished implementation shared at the user's explicit request for
independent testing. No PR or release has been created. All 51 finding groups
have partial corrections; **none is fully closed** against all grouped
requirements and final validation obligations. The additional model-deletion
toast issue is implemented and visually/native verified in both themes.

Read the entire [original report](RELEASE_AUDIT.md), its historical
[reproduction evidence](release-audit/), and the current
[findings matrix](RELEASE_AUDIT_MATRIX.md) before evaluating completeness.
Historical probes assert buggy behavior and must not become the passing
regression suite. Do not weaken assertions, delete tests or add unjustified
ignores. Preserve supported functionality and user data.

## Commit sequence

The implementation was developed before committing; this sequence was
reconstructed afterward by subsystem. Each implementation snapshot passed
`cargo check --locked --all-targets` in an isolated checkout of the staged tree.
This establishes compilation, not that each intermediate commit passed the
full test or lint suite. Some preparation commits have unused helpers until
their consumers arrive. No lint suppression was added for that purpose.

| Commit | Scope |
| --- | --- |
| `2089ad5` | Locked dependencies and authenticated Secret Service patch |
| `f17f47c` | Private filesystem operations and concurrent identity creation |
| `16fbf10` | Bounded task queues, worker ownership and subprocess cleanup |
| `bc6b3b1` | Database keys, schema/state transactions, redaction and retention |
| `3468a0d` | Sync trust, policy epochs, cancellation and discovery ownership |
| `367cd97` | Verified downloads, cancellation and atomic installation |
| `319da64` | Capture, hotkeys, provider contracts and clipboard delivery |
| `401068a` | UI/settings lifetime, asynchronous status and toast correction |
| `d8fe141` | Packaging, upgrade validation, notices and release gates |
| `c5c6bc7` | Disposable native and live-runtime regression probes |

Dependent API changes are grouped. The database commit adapts existing
fallible audio-clear/service-command callers; later capture/UI commits supply
their complete implementations. The capture commit also adapts the clipboard
capability field consumed by settings. The final implementation files are
byte-identical to those present before the commit reconstruction.

## Validation and safe reproduction

Recorded interim evidence lives in [release-audit-implementation/](release-audit-implementation/).
Older snapshots identify the former local branch and uncommitted source
diffs; preserve them as historical evidence. Packaging/runtime results apply
only to their recorded snapshots and configurations. They do not establish
that every required integration or final artifact has passed.

Required Rust checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --release --locked --bin mywhisper
```

The current production checkpoint `ce6b000` passes 263 Rust tests with five unchanged opt-in runtime ignores, formatting, strict Clippy, release build and current RustSec auditing. The later `0079d56` helper change makes CLI help side-effect free. Fourteen native UI/asset invocations across both configurations, fourteen packaging checks, real private-keyring/sync acceptance and a fresh Nix build/check phase pass. Actual private Ollama native load/unload and the new packaged GTK artifact smoke pass; registry CLI pull remains stalled. Earlier Whisper Tiny CUDA acceptance retains its exact earlier source checkpoint. See [current status](RELEASE_AUDIT_STATUS.md), [model-control evidence](release-audit-implementation/validation-review-model-load.json), [installer recovery evidence](release-audit-implementation/validation-review-install-recovery.json) and [CUDA evidence](release-audit-implementation/validation-review-cuda.json) for reproducible scope and retained failures. These are interim component checks, not completion of the release audit.

Use owned disposable databases, identities, keyrings, HOME/XDG directories and
synthetic speech/transcripts. Never reset, migrate, clear or prune real user
data. The provided smoke scripts create private fixtures and bound subprocess
lifetimes. Run shell scripts with `bash`; several are intentionally not
executable. Examples:

```sh
bash scripts/ui-smoke.sh
bash scripts/keyring-smoke.sh
bash scripts/cpal-smoke.sh
bash scripts/native-hotkey-smoke.sh
bash scripts/clipboard-smoke.sh
python3 packaging/test-release.py
python3 packaging/test-upgrade.py
```

Read each script's prerequisites first. Hardware, compositor and provider
results must identify the actual tested integration. Fixtures alone do not
close live integration requirements.

## Outstanding requirements

The matrix lists remaining obligations for every finding, including oversized-record sync segmentation and final protocol compatibility,
cross-process policy barriers, interrupted
migration and filesystem recovery, microphone ownership/device changes,
provider credentials and authenticated WSS/cloud contracts, remaining local
runtime/backend integrations, compositor/clipboard behavior, remaining UI lifetime checks,
and clean installation/running-service upgrades and final artifact checks.

No disposable cloud credentials have been identified. Do not assume that
fixtures establish live contracts or use real user credentials without an
authorized disposable test setup. The local host has Arch, niri, Nix and an
RTX 3090; a private Nix-store build and clean disposable Arch install/upgrade
have been exercised, but an actual clean NixOS desktop and all final artifact
gates remain outstanding.

The user explicitly deferred the project license: **do not add a project
LICENSE or invent a copyright holder**. Preserve third-party notices;
distribution remains blocked. Do not open a PR until all original submission
gates pass. Do not merge, deploy, create a release or create a release tag.


Reset follow-up (F12): schema 6 adds durable pending/completed reset ownership.
Schema-5 builds refuse the newer database; do not test a downgrade against real
user data. The reset worker keeps a private recovery intent across interrupted
cleanup and preserves edits after its DB transaction. Both UI smoke configurations
now include native reset lock/failure/retry and startup-recovery cases. See the
matrix and validation-review-reset.json for executed evidence and outstanding
warmup/filesystem/final-artifact requirements. No PR has been submitted.
