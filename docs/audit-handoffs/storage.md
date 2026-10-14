# fix(state): coordinate encrypted storage and authorized synchronization

Draft scope: R01, R02, R03, R04, R05, R07, R12, R16, R19, R20, R21, R22, R25, R41, R42, R43, R44.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `main`. Stable hosted-green SHA: `8603be1` (run 38589669408, checks success; supersedes `b20df2b`; #19 restacks from here).

Coupled foundation; migration/privacy/protocol acceptance incomplete. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Continuation (this draft, disposable fixtures only):
- Revocation/export/apply barriers: serve pre-handshake sync-off gate + per-accept recheck, exchange enabled/revoked check before record_seen, discovery refresh limited to known non-revoked rows, worker reap on idle + budget-exhaustion log.
- Clock: monotonic next_version via checked MAX+1 (overflow errors, no wrap); horizon rejection preserved; rollback errors preserved.
- Key/migration/fsync/ENOSPC: StorageFull/SQLITE_FULL mapped to recoverable storage-full with source preserved; migration fsync/rename/dir-sync mapped; stale -wal/-shm/-journal/-bak/-tmp retired post-rename.
- WAL/backup/audio: post-commit audio prune is best-effort (merge stays committed, idle maintenance retries); orphan sync_transfer_parts retired; clear_audio count fixed.
- Maintenance/fairness: retention policy still refuses incomplete settings (no permissive defaults); Coalescer FIFO+coalesce preserved, caps unchanged.
- Pairing/discovery/restart: mDNS guard extracted as should_refresh_addresses (tested); revocation stays local durable (gossiped revocation deferred to avoid wire change); older-build refusal + unknown-settings preservation untouched.
- Service threads use bounded runtime spawn_dedicated (Result, never panic); in-flight syncs abort via policy watcher (comment corrected).
- SyncEngine owns discovery handle + idle retention worker (joins/stops on drop); startup catch-up uses cancellable owned-epoch path; shutdown checkpoints best-effort (durability never depends on it).
- Authorization barrier: dedicated policy connection is query-only; file-identity mismatch aborts exchanges/watcher (replaced-DB safety).
- Capture path: deliver() uses atomic capture_settings snapshot + transactional persist_capture (vault via crash-safe replace); retention runs on every delivery; capture overflow is refused, never silently decoded.
- Reset path (corrected): foundation keeps pending-reset BARRIERS
  (capture/sync/settings gates refuse while pending) plus
  `reset_all_data`/`reset_on` (tombstones, revocations, sync-off
  defaults); the danger-zone reloads the fresh blob so sync can never
  be re-enabled by stale defaults. Two-phase nonce APIs, worker-owned
  recovery, Intent durability and GTK snapshot adopt relocate to
  #22/#23 with an add-back contract (source 08d2f6a; required
  acceptance: pending + blocked worker + second-process startup must
  not clear the barrier; failed FS cleanup stays pending and reports
  failure; interrupted retry finishes FS work without re-wiping
  post-transaction edits). No competing GUI reset implementation is
  added here to consume methods.
- Oversized-sync fixture is deterministic: byte-budget proxy cuts force interrupted rounds (generous round deadline); assertions unchanged.
- Migration staging file explicitly pinned 0600 before encrypted bytes land.
- Supervised process-launch API (ReaperMessage::Launch, PROCESS_LAUNCH, LaunchPermit, submit_process_launch + unit test) relocates to #22 with add-back contract (source: 08d2f6a; consumer: transcribe server spawn per snapshot:864); shared OwnedThread/spawn_session/reaper semantics untouched.

Gates here: cargo fmt clean; cargo clippy --locked --all-targets -- -D warnings clean (0 warnings); cargo test --locked --all-targets 195 passed + 0-test binaries ok, 5 ignored (incl. new pending-reset barrier regression + deterministic-oversized coverage); packaging/iteration/pr-test python gates pass; cargo audit clean after minimal rustls 0.23.45 -> 0.23.46 bump (RUSTSEC-2026-0346..0349, fresh DB; committed lockfile). Remaining external: release builds/cli/ui-smoke/shellcheck (CI runners); hosted 120s all-target runner still times out on segment/oversized CPU load under parallel CI contention — under measured investigation (byte-budget cuts landed; no timeouts enlarged, no coverage weakened).

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
