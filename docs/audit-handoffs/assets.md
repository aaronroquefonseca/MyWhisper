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
