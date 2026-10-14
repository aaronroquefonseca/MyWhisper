# fix(install): verify assets and recover interrupted installation

Draft scope: R49, R34 asset prerequisites.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-input-ownership`.

Active engine invalidation is wired by the session layer. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.

## Continuation evidence (PR #20 worktree session)

- `cargo check --locked --all-targets`: passes (warnings only; `-D warnings`
  still fails on dead-code shims owned downstream, e.g. `operation::guard`,
  `selection::Offer::receive`, `store` reset methods completed in #22).
- `cargo test --locked --lib`: 253 passed, 0 failed on the flatten/chmod
  hardening session (259 passed, 0 failed combined with the follow-up
  coverage below); 5 ignored are
  pre-existing live gates (network/hardware: `downloads_tiny_model`,
  `downloads_cpu_pack`, `live_oruk_offline_roundtrip`,
  `spec_thread_roundtrip`, `whistle_stream_roundtrip`).
- `scripts/installer-fs-smoke.sh` (real ENOSPC on private 512 KiB tmpfs):
  passes — previous bytes/inode preserved, private staging removed,
  authenticated synthetic retry promoted.
- Live `stt::tests::downloads_cpu_pack` (`--ignored`, real GitHub fetch):
  passes on the hardened GPU normalize/chmod path.
- New regression `artifact::tests::malformed_archives_and_declared_size_mismatches_fail_closed`:
  empty/random/truncated tar.bz2 and zip, hand-crafted `..` tar member
  (builder refuses `..`/absolute names at encode; absolute rejected by
  `member()`), FIFO entry, zip-slip member, and both declared-size
  mismatch directions all fail closed with no escape.
- New regression `installed_asset::tests::zero_byte_required_file_never_promotes`:
  empty staged payload blocks `install` before promotion; previous
  installation preserved.
- Fixes: `flatten_dir` now takes the installer authority check (both
  parakeet and sherpa call sites propagate instead of sherpa swallowing
  errors); GPU normalize observes `stage.check`, propagates `read_dir`
  errors, and only renames regular files; GPU/sherpa chmod paths use
  `symlink_metadata` so an archive link can never redirect chmod onto
  unowned files (GPU zip path rejects symlinks outright; belt and braces).
- Follow-up installer-side coverage (same disposable constraints):
  `artifact` malformed TAR/ZIP fail closed with staging/journal removed and
  previous install preserved plus authenticated retry; repeated cancellation
  over 4 TAR/ZIP rounds preserves previous content; stale observed
  authorities cannot authorize new staging after delete/reset and tampered
  epoch files fail closed; `installed_asset` delete/reset retires active
  verified content during use (`Operation::check` fails with
  "deleted, reset or changed"); `stt` every advertised model/runtime asserts
  pinned SHA-256 and authenticated specs with unique keys, and repeated
  cancelled downloads preserve existing files.
- Restack onto parent `c7479bc`: parent gated `private_fs::read_bounded`
  behind `#[cfg(test)]` and added production `read_private`; the 4
  production journal/key/receipt read sites migrate to `read_private`
  (permission-protecting; existing unsafe-permission and timestamp
  assertions unchanged). Parent's `.dns_resolver(HttpResolver)` additions
  are subsumed by the staged `artifact::download` path, which already
  owns its resolver. Combined `cargo test --locked --all-targets`: 265
  passed, 0 failed, 5 ignored; `cargo audit` clean.
- Still deferred, unchanged: automatic startup sweep (recovery on next
  locked retry/delete/cache-clear only), physical power loss, resource/
  admission and complete provider/UI acceptance. Active engine-retirement
  call sites remain integrated in #22 and must be reviewed together.

## API relocation ledger (strict-clippy path, no suppressions)

- Baseline strict clippy: 44 warnings. After the cuts below: 16, none new.
  Remaining owners: `operation.rs` contract (5, consumers #21/#22),
  retained `selection::Guard` (1, consumed by `Operation::guard`),
  `stt::model_present` (1, GUI status reader, supervisor to assign —
  candidate #23), `installed_asset::{trust_local, admit}` (2, semantics
  stay here; job-wrapper callers restore in #23), `store.rs` (3, #18),
  `runtime.rs` process-launch (4, session layer).
- `selection.rs` clipboard backend → #21 (agreed, PR21 comment
  6169568528). #20 retains the real `Guard` contract only
  (`pub(crate) type Guard = Arc<dyn Fn() -> bool + Send + Sync>`,
  constructed live in `Operation::guard`). Full implementation blob
  `0dfdf7a64a012d2f64ad8560cbb5bf8e3711db34` (907 lines incl. 4
  regression tests, which relocate with it — no assertions lost).
  #21 restack instruction: on conflict in `src/selection.rs`, take the
  #21 side wholesale; it must keep the `Guard` alias line byte-identical
  so `operation.rs:165` compiles. #21 base already contains the file;
  their `+40/-2` delta is preserved on their branch.
- `stt::{start_verification, start_local_trust}` GUI job wrappers → #23
  (no installer-side production consumers; verified by grep). Underlying
  `verify_spec/admit/trust_local/install/seal/adopt_reference` semantics
  and tests stay in #20. Exact removed block for #23 to restore verbatim
  (was `src/stt.rs` lines 674–704 at `169a1cf`):
  `start_verification(data, id)` builds the `model-/gpu-/sherpa-` key,
  observes authority, and dispatches `prepare_{model,gpu,sherpa}_pack`
  with `verification_only=true` via `start_artifact_download`;
  `start_local_trust(spec, id)` observes authority for `spec.key` and
  dispatches `installed_asset::trust_local` after emitting
  `DownloadEvent::Installing`. Both keep the `request?` early-return
  shape so revoked authorities never start work.
- Stable tested parent recorded: `c7479bc` (265-pass base at restack).
  No restack taken awaiting #19's published stable parent; new commits
  are fast-forward only.

## Startup sweep + power-loss recovery (independent installer work)

- `artifact::sweep_interrupted_installations(data)` (production, used):
  scans `.install/staging/*.json`, takes each key's asset lock
  non-blockingly (lockable ⇒ no live owner ⇒ stale; `WouldBlock` ⇒
  live ⇒ skip; missing/replaced control ⇒ preserve), revalidates the
  journal ownership record, rejects non-`Normal` record parents, then
  reuses `StagingDirectory::recover`. Returns recovered count;
  per-key mismatches preserve files. Wired opportunistically into
  `Stage::new_requested` (holds no locks yet; failures never fail the
  install) and published as the process-startup entry point for service
  startup (#18) to call once before serving requests.
- Probes (disposable tempdirs; one bounded child, 10 s deadlines):
  stale-vs-live-vs-tampered-vs-foreign sweep test (incl. proof that
  construction sweeps), parent-escape record rejection with sentinel
  intact, and a true SIGKILL probe — child stages partial content,
  parent kills, kernel releases locks, sweep recovers 1 with previous
  installation preserved.
- Live bounded matrix: `downloads_cpu_pack` (1.3 MB) and
  `downloads_tiny_model` (75 MB) both pass. All other advertised assets
  carry pinned SHA-256 + authenticated specs (unit-proven); remaining
  large downloads (CUDA 219 MB, Sherpa 255/25 MB, needle, 0.5–3 GB
  models) are external bandwidth/hardware prerequisites, not code gaps.

## Strict-gate ledger (hosted `release checks` runs `clippy -D warnings`)

- Hosted run `38578623513` tested `3756cb1`: fails at strict clippy only
  (dead-code-as-error), as expected while consumer restacks are pending.
  No assertion failures; fmt/check/tests/smoke/audit green there.
- Test profile: **zero** diagnostics after genuine `Operation` contract
  tests (`guard/cancel_flag/with_cancel/with_permission` share fate).
- Lib profile: 8 diagnostics, each with a downstream owner, none
  suppressible without stubs or deletion — `stt::model_present` (1,
  supervisor to assign, candidate #23), `installed_asset::{admit,
  trust_local}` (2, #23 wrappers restore callers), `operation.rs`
  contract + retained `Guard` (5, wired by #21/#22 provider integration).
  Hosted lint goes green as the stack restacks bottom-up; #21 can adopt
  the current stable SHA now (APIs below are frozen for consumers).

## v2 ownership distribution IMPLEMENTED (strict-green)

- Removed from this branch: `src/operation.rs` (contract + 3 tests,
  blob `eb7c429d`), `src/selection.rs` (Guard-only, blob `e7298a1f`),
  `admit` (`installed_asset.rs:251-268` at `f1da614`), `trust_local` +
  3 trust tests (`:674-741`, `:974-1053` at `f1da614`); `lib.rs` drops
  both `mod` lines.
- Rewrote 3 tests to direct-proof assertions with identical
  transitions: `legacy_comparison` (`admit`→`verify_spec` status map),
  `promotion_retires` (hand-built proof `unchanged()` directly),
  `delete_and_reset_retire` (`verify_spec` Missing/Invalid + direct
  `unchanged()`). No guarantee weakened; Operation-mediated boundary
  covered where really exercised (#22).
- Result: strict `clippy --locked --all-targets -- -D warnings` **zero
  diagnostics**; full `cargo test --locked --all-targets` **262/0/5**;
  smoke + audit green.
- Restoration (exact, for consumer restacks):
  - #22: `operation.rs` wholesale (`eb7c429d`, byte-identical there —
    explicit `git checkout <pre-restack-head> -- src/operation.rs`
    because silent deletion otherwise propagates); `admit` hunk
    re-applied into `installed_asset.rs`; trust_local stays OUT
    (deletion stands as move's first half).
  - #21: `operation.rs` deletes cleanly (verified untouched);
    `selection.rs` conflict takes #21 side wholesale (backend defines
    `Guard` byte-identical).
  - #23: canonical trust payload = #22
    `docs/audit-handoffs/gui-relocation-23/restore.patch`
    (byte-verified vs snapshot + `f1da614`) + `model_present` +
    `start_*` specs in this ledger; one minimal fixture helper
    duplicated there (helpers stay here for 9 untouched tests).
## Ownership relocation table (final dispositions, implemented)

| Symbol | Disposition | Owner | Preserving patch/commit |
|---|---|---|---|
| `stt::model_present` | **Moved** | #23 | Verbatim body in PR23 comment `6174231919`; restore above `model_status` |
| `stt::{start_verification, start_local_trust}` | **Moved** | #23 | Spec in API ledger above; restore verbatim |
| `selection.rs` backend + 4 tests (blob `0dfdf7a`) | **Moved** | #21 | Wholesale restore; agreement PR21 `6169568528` |
| `src/operation.rs` + 3 tests (blob `eb7c429d`) | **Moved** | #22 | Identical there; explicit checkout (silent deletion otherwise propagates) |
| `selection::Guard` (was `:14`) | **Moved** | #21 | Defined byte-identical by restored backend |
| `installed_asset::admit` (was `:253-268`) | **Moved** | #22 | Live caller transcribe:4451; re-apply hunk on restack |
| `installed_asset::trust_local` + 3 tests | **Moved** | #23 | Canonical: #22 `gui-relocation-23/restore.patch` (byte-verified) |
| verifier primitives + 14 tests | **Kept** | #20 | Production-called; suite rewritten without weakening |

## Full source manifest, regenerated on merged tree (never parent counts)

- Parent adopted by merge: `d4a82a8` (docs over code-tested `48a03a7`;
  code hosted run `38611165477` SUCCESS independently confirmed; doc/pin
  head run `38612411803` still pending at merge time — not claimed
  green). Conflicts resolved to parent side (input lane only).
- Own manifest, disposable resources, full logs in run outputs:
  `cargo fmt --check` clean; `cargo check --locked --all-targets`
  passes; `cargo test --locked --all-targets` **268 passed / 0 failed /
  5 ignored** on second full pass (first pass: 267/1/5 with
  `net::disable_and_reenable_during_handshake` Broken-pipe flake under
  parallel load — passes solo in 0.04 s; net lane, matches #18's stated
  1-in-4 environmental caveat; fail-path log preserved, not waived).
  `installer-fs-smoke.sh` passes on merged tree; `cargo audit` clean.
- Strict clippy: 7 lib diagnostics (owner-bound table above), test
  profile zero. #15 `e2751d2` nested-harness fail-path logic is absent
  from this branch and its harness lane (no cherry-pick across lanes);
  fail-path evidence is carried in the full run logs instead
  (sqlcipher negative-test errors, ENOSPC/SIGKILL probe output,
  contention-flake record above).
- #21 newer fixes noted and preserved untouched (`5f2f6b9` child-API
  follow plus paste certifications on `fix/audit-clipboard-ownership`,
  already restacked onto `367f54a`); later integration takes their side
  wholesale for clipboard files. Their branch is never reset here.

## Net handshake flake: root cause + evidence to #18 (PR18 `6175238052`)

- Failing test `net::tests::disable_and_reenable_during_handshake_never_exports_the_queued_snapshot`
  (`src/net.rs:2210`) under `cargo test --locked --all-targets` only:
  2 failures in 5 full runs; 20/20 solo and 8/8 concurrent isolated
  runs pass. Server panics at `:2257` (Verdict write, Broken pipe);
  client assertion at `:2280` fails (wrong error string).
- Root cause: missing client-parked barrier. The toggle fires when the
  *server* finishes Hello (`:2250`), but the *client* may not have
  passed its post-Hello `sync.enabled` gate (`exchange()`, ~`:686`).
  `save_settings` always bumps `policy_epoch` (`store.rs:936`), so the
  intended epoch-detection path only runs if the client is already
  parked in the exchange; otherwise it fails closed with
  `sync disabled`, drops the socket, and the test server's `unwrap()`
  panics on Broken pipe. Fix belongs to #18 (barrier/test-hook and/or
  fail-closed test server); assertions preserved, zero `src/net.rs`
  changes here (byte-identical to parent — verified empty diff).

## Adopted storage-lane fixture patch + own-load validation

- Adopted `b842a0d9` (storage lane, hosted-green upstream): cfg-gated
  `sync-client-parked` hook in `exchange()` + bounded/loud fixture
  accepts/rendezvous/joins. Cherry-pick verified hunk-identical,
  `src/net.rs` only; installer scope, APIs and inventory untouched.
  No foundation restack, no new runner (predecessor `631fe0e` runner
  correction stays in its lane).
- Own-source validation: **3 consecutive full parallel
  `cargo test --locked --all-targets` runs, 268/0/5 each**, plus
  targeted handshake pass. Failure stays open only pending hosted
  confirmation, not local evidence.
- Hosted run `38616713788` (source `f9c790b`) inspected: fails at the
  strict-clippy step with exactly the 7 owner-bound diagnostics above
  — zero assertion failures, race fix not contradicted. Hosted lint
  greens when consumers wire (merge order), as recorded.
- Runner: partition scripts inherited via parent merge; local runs use
  `cargo test` directly. #15 `e2751d2` nested-harness logic stays in its
  harness lane (no cross-lane cherry-pick); fail-path logs captured in
  run outputs instead.
- Inventories retained: branch HEAD recorded per push comment;
  `scripts/test-manifest.txt` as merged from parent (unmodified);
  original source inventory owned by the tracking PR (immutable).
- The 7 lib lint diagnostics stay OPEN (owner-bound table above);
  layer acceptance is NOT claimed passing until consumers wire.
