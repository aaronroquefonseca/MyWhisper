# GUI-consumed implementation relocation → #23 (from #22)

Supervisor ACCEPT (no suppression, no fake callers, exact preservation).
Sources: immutable snapshot `08d2f6a` (byte-exact blocks) + pre-deletion
blobs in `source-blobs.txt` (`source-head.txt`).
Base for `restore.patch`: current #22 head (verified `git apply --check`
clean). #23 pins its old parent while preparing, then applies its new
parent plus this restoration as one child update and runs full gates
before publication.

## Apply (in #23, from repo root, after rebasing onto new #22 parent)
`git apply docs/audit-handoffs/gui-relocation-23/restore.patch`
(paths are `src/...`). Then run fmt + strict clippy + full lib gates.
`mod maintenance;` and `src/maintenance.rs` come back with the patch;
no other #23 step needed for these symbols.

## Moved symbols (prod callers all in #23-owned GUI files)
- `src/store.rs`: `Store::adopt_settings_baseline` (stays `impl Store`;
  no broad API exposure) ← `app_settings.rs` snapshot adopt.
- `src/stt.rs`: `start_verification`, `start_local_trust` ←
  `app_settings.rs` asset dialogs. `start_artifact_download` stays in
  foundation (used by `start_download`).
- `src/maintenance.rs`: whole file (`RetentionMaintenance::{start,spawn}`,
  `Drop`, tests) + `mod maintenance;` in `src/lib.rs` ← `app.rs`
  application state.
- `src/installed_asset.rs`: `trust_local` + its 3 tests
  (`explicit_local_trust_preserves_manual_content_and_detects_later_corruption`,
  `local_trust_never_attests_missing_empty_or_non_executable_files`,
  `queued_verification_and_trust_cannot_renew_deleted_or_reset_authority`)
  ← via `start_local_trust`. Coordinate with #20 (ancestor copies
  unaffected; #20 decides its own relocation).
- `src/runtime/shutdown_tests.rs`: async-copy denial assertion
  (`copy_text_async("late synthetic", true).is_err()`) moves to #23's
  shutdown cover; the sync-path denial assertion stays here. Coverage
  preserved, not weakened.

## Explicitly NOT in this payload (carried in #22 foundation)
- `CopyTask`/`poll`/`copy_text_async`: WIRED in #22 as the async
  capture-edge copy (`transcribe.rs` keep-mode leg). #23 inherits.
- `stt::model_present`: carried in #22 for inference admission
  (`transcribe.rs` caller); #23 inherits it for status UI.
- `reset::start`/`Job`/`Event`: service-driven entry (`reset` server
  command → `Recovery::start_interactive`); #23 GUI contract is
  `set_server_cmd("reset")`, never a parallel worker.
- `src/reset.rs` worker/barriers: `run_owned`, `Recovery::{tick,drain,
  start_interactive}`, `requested`, intent/nonce protocol,
  `Event::{Snapshot,Finished}`, `Job{events}`, `start` — single worker
  core in foundation. Interactive entry is service-driven: the GUI posts
  a `reset` server command (same pattern as stop/start; see `service.rs`
  `reset` arm), the service calls `Recovery::start_interactive`, and
  outcomes drain through the resume path. #23 GUI contract: replace any
  direct `reset::start` call with `set_server_cmd("reset")`; drive
  interactive resets only through the service, never a parallel worker.
  No competing worker, no suppression, no visibility widening.
  #18 `complete_pending_reset` boot shortcut and `reset_worker`
  FS-error suppression are BLOCKED (see #22 report); #19 must not
  restack until the corrected contract is stable.

## #23 snippet failures note
#23's 2 failures are the stale-base `FakeCleanup` shape (missing
`done/done_reason`) against the completed-answer contract. After
integrating current parent (#22 head carries the completed fixture
plus #17 `8eabfaf` hardened parsers) they must pass unmodified —
not waived as baseline.
