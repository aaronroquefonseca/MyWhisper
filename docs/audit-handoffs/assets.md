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
- `cargo test --locked --lib`: 253 passed, 0 failed; 5 ignored are
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
- Still deferred, unchanged: automatic startup sweep (recovery on next
  locked retry/delete/cache-clear only), physical power loss, resource/
  admission and complete provider/UI acceptance. Active engine-retirement
  call sites remain integrated in #22 and must be reviewed together.
