# fix(state): coordinate encrypted storage and authorized synchronization

Draft scope: R01, R02, R03, R04, R05, R07, R12, R16, R19, R20, R21, R22, R25, R41, R42, R43, R44.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `main`.

Coupled foundation; migration/privacy/protocol acceptance incomplete. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Continuation (this draft, disposable fixtures only):
- Revocation/export/apply barriers: serve pre-handshake sync-off gate + per-accept recheck, exchange enabled/revoked check before record_seen, discovery refresh limited to known non-revoked rows, worker reap on idle + budget-exhaustion log.
- Clock: monotonic next_version via checked MAX+1 (overflow errors, no wrap); horizon rejection preserved; rollback errors preserved.
- Key/migration/fsync/ENOSPC: StorageFull/SQLITE_FULL mapped to recoverable storage-full with source preserved; migration fsync/rename/dir-sync mapped; stale -wal/-shm/-journal/-bak/-tmp retired post-rename.
- WAL/backup/audio: post-commit audio prune is best-effort (merge stays committed, idle maintenance retries); orphan sync_transfer_parts retired; clear_audio count fixed.
- Maintenance/fairness: retention policy still refuses incomplete settings (no permissive defaults); Coalescer FIFO+coalesce preserved, caps unchanged.
- Pairing/discovery/restart: mDNS guard extracted as should_refresh_addresses (tested); revocation stays local durable (gossiped revocation deferred to avoid wire change); older-build refusal + unknown-settings preservation untouched.
- Service threads use bounded runtime spawn_dedicated (Result, never panic); in-flight syncs abort via policy watcher (comment corrected).

Gates here: cargo check --locked --all-targets passes (17 pre-existing dead-code warnings for downstream layers); cargo test --locked --lib 195 passed, 5 ignored; 6 new disposable regression tests pass. Strict clippy -D warnings and live/native gates remain (expected for isolated layer).

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
