# Integrated acceptance — final source 6d6da5e3 (PR #15 coordination)

Full SHA: `6d6da5e35a36976d351f55a9f81f41f1cf3f73e4`
Branch: `fix/audit-settings-lifecycle`. Hosted run 38638287770: SUCCESS.
Parent db1f4e5 integrated. MIT license, v0.9.4 preserved. No file
deletions vs main (115 files changed, all additions/modifications).
This record is source-specific to the SHA above; nothing here is
main-only evidence. Draft held; no merge, tag, release or deploy.

## Gate evidence on this exact source

| Gate | Evidence |
| --- | --- |
| Hosted checks job | SUCCESS: fmt, strict Clippy locked all-targets, 4 batches 340 passed + 5 ignored exactly once (345 manifest + 9 harnesses), audit, release/upgrade/CLI, UI, candidate |
| Release candidate | clean (`source_dirty: false`, `source_sha` = head), asset sha256 `68339cdc…f5a928` |
| validate-artifact + SHA256SUMS | both pass on the candidate |
| No-keyring CLI | 2/2 pass |
| cargo audit --deny warnings | exit 0, 0 vulnerabilities (fresh DB at run time) |
| Arch packaged install + pkgrel upgrade | pass in rootless Podman clean userspace |
| Nix symbolic-logo / desktop VM / whisper-vulkan-loader | exit 0 via owned-store runner (own gate/build dirs, no host daemon change); desktop VM covers System dark, GUI close, encrypted storage, repeated user-service start/stop |
| Native UI smoke (xvfb, both themes) | pass; one local run tripped the 500ms provider-IO-close assertion in ollama_load_smoke (fixture's own 500ms socket read timed out waiting for EOF after page removal; production has no 500ms close contract → readiness-fixture defect, correction proposed to #23 scope: bounded retry under the 5s stall guard asserting closure, not closure-within-window). OPEN until hosted demonstrates it; positive locally reproduced on e39c62e (both themes exit 0, outer bound intact), negative evidenced on 6d6da5e3 |
| test-release.py / test-upgrade.py | 11/11 and 5/5 pass on this source |
| Integrated diff review | 46 src files, +31117/-4466; deps added only for owned scopes (wayland protocols, vendored secret-service 5.2.0 patch, glib-unix, rtrb, socket2/async-io/futures-util, tokio time); no gratuitous expansion; Operation/admit/CopyTask/Guard/trust present |

## R01–R51 grouped status (owner scopes, this source)

- UI ownership (#16): hosted green incl. broad/focused UI; local smoke green with one timing flake noted above.
- Provider contracts (#17): hosted green reported.
- Storage/sync (#18 chain): 200-scope gates green; checkpoint used in migration path (legacy checkpoint before journal change) and owned shutdown; migration-boundary evidence per owner.
- Input ownership (#19): hosted green at d4a82a8 (225 exactly once); nested-stdout fixture correction landed.
- Verified installers (#20, 916a45c): hosted green/strict-zero reported.
- Clipboard ownership (#21, ed081818): hosted green reported.
- Session lifecycle (#22, db1f4e5): integrated as parent; Operation/admit/CopyTask preserved by source evidence; shutdown-checkpoint wired.
- Settings lifecycle (#23, this head): trust + 3 tests restored; SKIP-bypass corrected and independently verified (R51 section in LEDGER).
- Release/packaging R48/R49/R51 (#15 coord + #20): x86_64-only authenticated recipes, fail-closed unvalidated hosts, artifact integrity proven above.
- First-run identity R06 (#15): atomicity coordination recorded in FINDINGS matrix.

## Migration/privacy/cleanup consequences (this source)

- Schema `user_version = 8`; plaintext→encrypted migration with legacy WAL checkpoint-before-journal-change and close-and-retry errors; identity preserved, malformed kept as errors.
- Runtime pragmas: WAL + secure_delete + foreign_keys; checkpoint API reports busy-reader blocking instead of silently skipping.
- Privacy reset clears trust/tombstones and forces `keep_audio_recordings = false`; audio retention is retention-days gated with immediate purge on toggle-off.
- File logging bounded; tmp/state/cache dirs private 0700; downloader staging atomic; no SKIP or unauthenticated artifact path remains.

## Still mandatory: live/external gaps (no substitution)

Running desktop/service upgrade + recovery, GPU downloaded-runtime
inference, trusted authenticated WSS, paid provider credentials,
physical microphone/device-change, cross-host LAN pairing/revocation,
non-niri compositor variants, R49 remaining advertised assets. None
demonstrated on this source; fixtures and unrelated-head passes do
not substitute.

## Live-gap coordination (route owned by #15, plans owed by owners)

- #21 asked for an executable guest-only paste-check plan (nix check attribute, guest ydotool/uinput fixture through the real paste path, exact prereqs); host input/clipboard/keyrings stay isolated.
- #18 asked for an executable two-VM-peer pairing plan (two-node nix check, real Noise/discovery/sync/revocation between packaged executables with synthetic credentials, exact prereqs).
- Runnable plans will run within resource bounds once posted; stub providers/protocol fixtures do not establish integration. No physical/cloud/GPU coverage fabricated.

## Joint-acceptance gate: PR #11 NOT CLEAR

All 12 prior independent probes pass at bcaff782, but one
settlement-admission race remains; v14 follow-up relayed to the
original implementation owner. Joint acceptance stays open pending
exact-head review of that fix. No final joint acceptance claimed.
