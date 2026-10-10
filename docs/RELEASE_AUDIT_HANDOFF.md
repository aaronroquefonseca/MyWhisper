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

The current suite contains 165 passing tests and five unchanged opt-in runtime
tests. Current RustSec auditing recorded zero vulnerabilities and warnings.
Native UI smoke passed light/English and dark/Spanish with fatal GTK criticals;
an earlier intermittent dictionary-dialog focus critical remains under
investigation despite subsequent passing runs.

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

The matrix lists remaining obligations for every finding, including paginated
sync/protocol compatibility, cross-process policy barriers, interrupted
migration and filesystem recovery, microphone ownership/device changes,
provider credentials and authenticated WSS/cloud contracts, remaining local
runtime/backend integrations, compositor/clipboard behavior, UI focus lifetime,
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
