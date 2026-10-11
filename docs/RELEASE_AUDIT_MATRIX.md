# Release audit implementation matrix

Baseline: `f018ab855af41759e275c80708d67973f2890587`. Implementation branch: `fix/release-audit`. Audit branch/evidence remain untouched. **Work in progress; no finding is closed and no PR has been submitted.** The user authorized publishing this branch for independent testing before completion. See [the testing handoff](RELEASE_AUDIT_HANDOFF.md) for the reconstructed commit sequence and outstanding gates. Historical validation snapshots retain their original branch names and uncommitted-source digests.

Progress counts: **51 total finding groups** (17 P1, 33 P2, 1 P3); **51 have implemented corrections**, **0 are fully closed** against every grouped requirement and final validation obligation. Implemented does not mean complete: remaining work is listed for every finding. The additional deletion-toast issue is outside these 51.

Additional user-reported issue: model-deletion feedback had an inverted light toast in the dark theme and a rectangular close hover. Corrected in `app.css`: active card/text colors and a compact circular close control. Native computed-color, rendered-background, hover-feedback and dismissal assertions pass in light and dark; rendered images reviewed: [dark hover](release-audit-implementation/toast-dark-hover.png), [light hover](release-audit-implementation/toast-light-hover.png). Covered by `ui_preview --toast-proof`; included in this same implementation/PR scope.

## Independent review of f426c9a

The [complete independent review and original probe sources/logs](release-audit-implementation/review-f426c9a/REVIEW.md) are preserved as historical failure evidence. Ten observations were reproduced; two were inspected code paths. These expose remaining requirements in the original finding groups rather than replacing their acceptance criteria. All twelve are initially open. Historical probes must not be counted as passing regressions. Subsequent corrections, executed coverage and compatibility consequences are recorded here as work proceeds.

| Review issue | Original groups | Root cause and required correction | Coverage and status |
| --- | --- | --- | --- |
| F01: uploads after cancel/reset | R05, R13, R29, R38, R39 | Provider traversal outlives capture authority. One monotonic authority carries cancellation, the original durable capture generation and the identity of the opened database across capture/finalization/delivery. HTTP transfers own a cancellable async runtime and bounded resolver; WS guards admission to each write and owns active socket interruption; CLI work owns its process group and staging. | Correction passed for provider IO: 24 full-pipeline blocked-header/partial-body STT and cleanup cases exercise Cancel and separate-process Reset, privacy off/on, Clear History, mute off/on, and policy-read failure. No fallback is contacted, no history/audio is retained, active peers observe closure, and completion fires once within 500 ms; unreadable policy reports Failed and stays denied after recovery. Two partial-upload cases stop before the whole body is sent; twelve HTTP DNS cancellations and actual stalled NSS recover under their budgets. Four authenticated plain-WS cases reject new audio after reset/cancel and interrupt completion; four file/FIFO CLI cases reap children and remove private staging. Queued markers preserve explicitly enabled discard duration but cannot recreate deleted/replaced databases or repopulate reset data. Authorized HTTP fallback remains supported; normal capture cleanup cannot cancel its finalization phase. Full Rust suite: 189 passed, zero failed; formatting/strict Clippy, release build and both native UI configurations passed. Real Tiny Whisper CPU download/inference/cleanup passed. Real Nemotron CPU streaming was repeated after this correction and passed on a known synthetic fixture. Provider startup now has the F05 owned readiness/lease correction and service admission has the F03 worker-pressure correction; remaining microphone/input-edge requirements, authenticated WSS success/live cloud contracts and final privacy/artifact review remain open. |
| F02: missing settings grant permissions | R21, R50 | Permissive serde defaults and swallowed database read errors conflate fresh settings with incomplete existing state. Add conservative versioned decoding and fallible reads; preserve original JSON/unknown fields and visible recovery. | Correction passed: six regressions cover missing whole sections/individual permission fields, actual SQLite read failure and recovery, malformed/future/missing configuration, schema-4 migration with user data preserved, unknown root/nested/model-entry fields through edits, and unknown retention policy. Before-fix checks failed on permissive defaults and lost unknown fields. Schema 5 initializes documented defaults only for a genuinely new database; existing missing configuration recovers with permissions disabled. Invalid/missing retention policy cannot authorize automatic deletion; explicit valid retention still works. Both real GTK configurations show the recovery message and preserve disabled permissions/unknown fields after a native settings save. Full Rust suite: 176 passed, zero failed; formatting/strict Clippy passed. Atomic authorization follow-up also rejects missing whole sync sections, missing/null/false permission, future settings versions and deleted settings rows during actual export and transactional import, with the previously captured generation unchanged. Rejected imports leave no partial dictionary writes; restoring explicit permission preserves the synthetic transcript and restores export. Before-fix export disclosed speech; corrected full suite passes 192 tests with five existing opt-in ignores, formatting/strict Clippy and release build. No schema or peer protocol change. Interrupted migration and final packaged compatibility remain open. |
| F03: queued press loses speech | R08, R28, R33 | Reserving an epoch does not arm capture; preparation waits behind short workers. Own raw capture at the input edge before IO, preserve it through delayed preparation/release, and explicitly fail rejected admission. | Worker-pressure correction: service input uses a startup-provisioned, non-waiting read-only policy connection and arms directly, without submitting press admission to the short pool. One read transaction snapshots current configuration/authority without dictionary/history enumeration; each session has separate cancellation. The regression holds all four workers through exact PCM capture, release, inference and one Complete callback, then twelve repeated cancel/new-press cycles and saved mute before watcher refresh. Admission errors show Error rather than claiming successful capture. 201 Rust tests pass, five existing opt-in ignores unchanged; formatting, strict Clippy and release build pass. No schema/protocol change. Original input-edge/microphone readiness, nonzero delay, device-change and final native integration requirements remain open; the legacy split reserve/begin API is not a deferred-capture queue. |
| F04: arm IO blocks release/cancel | R08, R10, R50 | ARM_LOCK spans database/keyring IO. Move IO outside the ownership lock and validate the generation when publishing; invalidation must be prompt. | Correction passed: held database initialization lock blocks preparation while release, cancel and reset invalidation each return within 100 ms. The regression waits for the actual opener descriptor, then confirms no late recorder/live session or stale preview after IO resumes. Before-fix release blocked for 311 ms. Database/keyring/plan IO runs outside ARM_LOCK; only the still-owning generation can publish success or failure. Background arming uses open_existing so it cannot recreate a removed database. Full Rust suite: 178 passed, zero failed; strict Clippy passed. Live stalled Secret Service timing and the new capture-admission ownership changes in F03 remain open. |
| F05: warmup kills active server | R15, R34 | Replacement ignores accepted recording/finalization users. Lease engines and defer replacement or explicitly cancel owners with supported outcomes. | Correction passed: an exclusive owner for the supported local runtime slot spans readiness and inference (including streaming/speculation), while optional warmup coalesces one deferred request. Sessions wait with cancellation and existing budgets, preserving their original configuration. Six changed language/model/backend/address/port/runtime requests cannot kill active inference; latest valid warmup applies after completion. Full capture/finalization/delivery retains the original Spanish transcript and one Complete callback. All four short workers remain occupied through cold startup, inference and deferred replacement. Unique startup instances prevent old cleanup from removing a new child of the same configuration. Twelve canceled lease waits and six actual online/offline readiness Cancel/Reset/Stop cases release ownership and reap children within their bounds. Queued startup retains the control generation; Stop retires deferred requests. Supervised children are created by the existing persistent ownership/reaper thread: one launch permit, no increased pool/session counts, no dependency on short-worker availability, and no temporary-thread PDEATHSIG kill. Launch panic/admission recovery is covered. 205 Rust tests pass, strict Clippy/fmt/release, current security audit, seven native hotkeys and real Tiny CPU download/inference/cleanup pass. No schema/key/peer-protocol change; redundant Whisper /load is removed because --model and the full configuration key already own model selection. The follow-up R46 correction passes both native UI configurations with fatal criticals and repeated focus destruction; its before-fix reproduction is retained. Idle rebind listening-socket checks, more runtime/backend readiness integrations, keep-warm duration and final ownership/artifact review remain open. |
| F06: discarded rows retain speech/diverge | R03, R04 | OR flags do not canonicalize discarded payloads. Redact text/raw/words on insertion and merge whenever discarded; retain supported marker duration and migrate existing rows. | Correction passed: all six normal/discard/invalid-discard permutations repeated three times, schema-3 migration/reopen, and encrypted sync in both orders followed by deletion. Schema 4 redacts existing discarded payloads and preserves duration; older schema-3 builds refuse the newer database. Before-fix regression failed on retained speech. Full Rust suite: 168 passed, zero failed; strict Clippy passed. Final migration/privacy/artifact review remains open. |
| F07: larger histories cannot sync | R44, R03, R05 | Whole-state export lacked paging/deltas; individual imported records could still exceed one message. Bound database and wire batches, negotiate resumable record segments, and couple publication to transactional cursor ownership. | Aggregate pages/deltas and individual-record segmentation are implemented. Schema 8 stages 64 KiB SHA-256-checked segments inside the encrypted database. Receive offsets commit before ACK; only a complete validated record and its receive cursor merge atomically. Lost ACK/reopen, competing rounds, source deletion, reset, clear, retention, revocation and loss of trust preserve deletion/authorization semantics and remove staged bytes. The actual signed Noise test transfers a >16 MiB individual Unicode record through a throttled TCP proxy and repeated two-second interrupted rounds, then verifies exact data and zero remaining fragments. Ten store regression groups and that network regression pass; full locked suite: 242 passed, zero failed, five original opt-in ignores unchanged. Actual schema-7/current signed Noise compatibility passes both directions; SIGKILL before migration commit preserves usable schema 7, subsequent upgrade preserves data, and the actual old build refuses schema 8 without changing its encrypted file. No production deadline, frame cap or worker increase; TCP_NODELAY removes delayed-ACK buffering in alternating length-prefix/ACK frames. Explicit limits: 64 MiB decoded record text, 512 MiB aggregate encrypted staging, 16 active transfer states, one hour idle staging lifetime; unsupported oversized records remain local with a visible error. **Open:** manual/inbound/cross-process scheduling fairness, scale/resource/privacy review, tombstone retirement policy and final packaged/live integration gates. Evidence: validation-review-segments.json and segments-evidence/. |
| F08: WS DNS bypasses deadline | R13, R36 | Blocking NSS precedes owned socket deadlines. Reuse bounded owned resolution/connect and propagate cancellation through TLS/upgrade/streaming. | Correction passed: the review's process-private NSS shim fails the before-fix deadline regression at 11.000 s; owned getent resolution now returns within 4 s (3 s resolver budget). Twelve barrier-controlled resolver cancellations complete within 500 ms and restore task capacity. Actual protocol completion succeeds after an IPv6 refusal followed by IPv4 resolution, with the synthetic bearer header intact. Twelve additional cancellation rounds cover plain upgrade, TLS ClientHello, blocked 8 MB upload and completion; each returns within 500 ms and its peer observes closure. All eight existing session slots recover, and excess admission remains rejected. Shared connect tests cover a real stalled TCP backlog. The session owns its watchdog through IO/completion, with absolute progress deadlines and no unbounded resolver threads. Full Rust suite: 181 passed, zero failed; formatting/strict Clippy and both native UI configurations passed. Real authenticated WSS success/live-provider contracts remain open; durable policy and HTTP cancellation coverage is recorded in F01. |
| F09: corrupt assets appear installed | R35, R49 | Presence/95%-size checks and cached UI status were not execution authority; loaded runtimes lacked asset ownership. | Provider admission now freshly verifies every managed model/runtime before execution; the 96%-length Whisper reproduction is rejected before a server starts while retaining the file. Speculative, streaming and release inference retain verified snapshots and original request epochs; deletion/reset/replacement revoke IO, retire only the owned server instance and cannot authorize a queued request under a replacement path. Forever-warm servers also retire invalid asset proofs. Legacy archives can be compared with an authenticated normalized reference without replacing files or inodes. Deliberate manual/quantized installations remain supported through explicit local-trust confirmation and a distinct installed-state label; a local attestation does not claim upstream authenticity, and later changes are rejected. Seven additional regression groups pass; full suite 231 passed with five original opt-in ignores unchanged. Strict lint/fmt/release, both existing native configurations, current RustSec, 14 packaging checks and fresh real Tiny CPU download/admission/inference/cleanup pass. Existing authenticated receipts remain compatible; no database/identity/keyring/peer migration. Dedicated native acceptance now passes in light/en and dark/es: actual pinned CPU runtime reference comparison preserves executable bytes/inode; native cancel creates no manual receipt; confirmed local trust preserves synthetic manual model bytes and displays the explicit local-authenticity label; same-length modification removes installation authority; callbacks/heartbeat remain responsive. The preview initializes the existing runtime, so these checks exercise real verification workers. Full suite: 243 passed, zero failed, five original opt-in ignores unchanged; strict lint/fmt/release, all eight existing native invocations and 14 packaging checks pass. **Open:** interrupted repair/promotion/disk-full/extraction, complete queued cross-process installer/DNS ownership, remaining runtime/manual/resource integration review and final packaged/live gates. Evidence: validation-review-assets.json, validation-review-asset-ownership.json and validation-review-asset-dialogs.json. |
| F10: thinking model-default overwritten | R38, R50 | Every load treats explicit null as a missing legacy field. Version missing-field migration and preserve all three explicit choices in UI/service requests. | Correction passed: None/true/false persist through store reload, all three real GTK save choices pass in both smoke configurations, and captured HTTP requests omit think for None or send the explicit boolean. Migration applies only to the legacy Ollama entry shape with an absent thinking field; explicit null stays model default, and the field is persisted on save. The previous every-load rewrite is removed. Before-fix regression failed on None becoming false. Broader live-provider/final artifact gates remain open. |
| F11: startup ignores saved mute | R23 | Listener publishes before initial input/feedback preferences; watcher sleeps before first pass. Apply settings before accepting input and start polling immediately. | Correction: startup applies persisted mute, cue and media preferences before publishing the listener; watcher runs immediately and retains a bounded cadence on error paths. A process-isolated encrypted save/reopen check verifies first input is muted, sound disabled/media preference applied, and explicit re-enable restores ordered input. The native gate repeats initial muted input through its exclusively owned uinput device and the production poller. Full Rust suite: 177 passed, zero failed; strict Clippy and seven native hotkey checks passed. Complete headless-service launch timing and actual sound/MPRIS integration remain open; the native poller check is not a full service launch. |
| F12: reset blocks GTK/stales UI | R05, R50 | Confirmation performs DB/reset/installer lock/recursive deletion synchronously and reconciles only on full success. Own reset in background, coordinate captures/installers/servers, always reflect committed state and recover partial cleanup. | Correction passed for reset ownership/recovery: the original native GTK callback blocks 5.006 s on a separate process’s DB lock. The owned worker now admits without database/file IO on GTK, reconciles worker-loaded committed settings even if cleanup fails, and exposes pending/cleanup-retry states. Capture, atomic sync export/import and installer admission/promotion deny a durable private reset intent before the DB reset commits. The DB tracks a pending nonce and completed nonce; retry/restart never repeats a committed reset or wipes subsequent edits. Eight regressions cover held DB/installer locks, active independent-process installation, four interruption states including a missing file marker, killed coordinator processes, corrupt/foreign recovery records, replacement after commit, preferences/dictionary preservation after failure, and schema-5 migration. Both real GTK configurations exercise held independent-process locks, an active heartbeat, partial filesystem failure, committed theme/language reconciliation, disabled pending permission controls, retry and startup recovery. Schema 6 protects the pending-reset semantics: an actual schema-5 build refuses it without modifying the encrypted file; the current build upgrades/reopens preserved data. Current Rust suite: 200 passed, zero failed; formatting/strict Clippy, release build, current security audit, private real keyring/sync lifecycle, seven native hotkey checks and 14 packaging checks pass. No peer protocol or key/identity rotation. Owned local warmup/readiness in F05, final filesystem/privacy/cleanup review and the final packaged artifact remain open. |

Main integration: merged `15776960fe4351be551df607861db21b59d0e388` (0.9.3) into this implementation branch. The Nix conflict retains source builds and reads the Cargo version. Arch uses the actual authenticated 0.9.3 archive SHA-256 `443ce5d00a992bf1cca6c4cbadd5978216b9a7d62a7661cce23c6ada35a11046`, verified by download. Locked all-target compilation and 14 packaging tests pass after this integration; final builds/native/artifact checks must be repeated after corrections. Main and the historical audit branch are unchanged.

Interim validation (not the submission gate): locked all-target Rust suite passed 165 tests; existing five opt-in downloaded-runtime tests remain unchanged. Strict Clippy, formatting and current security checks pass. Both native UI configurations pass widget release, capture nonce/suspension, physical punctuation and Spanish language preservation and retention controls with storage disabled/custom values checks. Real CPAL/ALSA passes F32/I16/U16 with 1/2/6 channels at 48 kHz using a process-private paced synthetic file source; this does not establish physical-device change handling. Source-built Nix artifacts and a disposable Arch package install/pkgrel upgrade pass, including the 164-test keyring/status snapshot before the Cohere implementation; all final artifact checks must be repeated after remaining changes. Current RustSec audit reports zero vulnerabilities and warnings (1,296 advisories, database commit `7eebec69c352c7191b1f13eb95dd510eeca5d1de`, 2026-10-09). Actual verified upstream download/inference gates passed for Tiny Whisper CPU/Vulkan/CUDA (verified disposable CUDA 12/cuBLAS libraries and owned GPU allocation), Whistle file/FIFO streaming, Parakeet TDT v3 CPU, and Nemotron Speech streaming CPU. Real disposable Secret Service passes eight independent concurrent creators and missing-key refusal/recovery. A disposable native niri/wtype/GTK target passes delayed consumption, empty/text/binary restoration, user replacement, cancellation and copy-only/no-op behavior after the nonblocking socket/actor changes. Other integrations, final artifact and the complete audit implementation remain outstanding. Historical audit probes are unchanged.

Latest packaged checkpoint: committed source `cc0e751` builds successfully in the private Nix store, passes all 243 sandbox Rust tests with five original opt-in ignores, collects all 370 dependency notices, and reports version 0.9.3. The actual wrapped package starts a visible GTK window, closes normally with zero exit, and creates only encrypted disposable storage under a private D-Bus/Xvfb/bubblewrap session without audio/input devices. The fresh real private keyring/sync lifecycle also passes after the settings-write ownership correction. This is a local Nix-built artifact on the Arch kernel/display stack, not complete clean-NixOS desktop or every final artifact integration acceptance. See [current artifact evidence](release-audit-implementation/validation-review-final-nix.json). Remaining grouped obligations and live gates below still apply; no PR submitted.

Each finding retains its original classification. Grouped subissues below quote the complete audited defect description so no grouped defect is dropped. Implemented corrections and remaining obligations are recorded separately; a passing fixture does not close a live integration gate.

## R01 — Sync off still allows transcript transfer

**Grouped subissues and baseline evidence:**

**Reproduced.** [net.rs](../src/net.rs), `sync_all_trusted` at line 732, loads settings but does not check `sync.enabled`. `sync_after_change` and delayed startup sync invoke it independently of the listener's stop flag. Dictionary edits and history deletion call this path from the GUI. Turning off the sync engine therefore does not stop outgoing synchronization, including sending complete transcript history. The probe saved `sync.enabled=false`, performed encrypted loopback sync, and verified the transcript on the receiving database.

- Root cause/correction: Master setting, local trust and a captured authorization epoch govern queued, accepted and outgoing operations. Authorized export uses one read transaction; apply uses the same authorization check inside its write transaction. A policy monitor shuts down an active socket when the captured epoch changes. Change/startup requests carry the epoch from their original writer connection; engine-origin requests also retain that engine's cancellation token. TCP connection/handshake progress checks cancellation without extending their deadlines. Background database opens cannot recreate a removed database.
- Regression coverage and executed validation: Encrypted loopback passes, including off/on during a handshake barrier and disable during a blocked encrypted 5 MiB export; the stale operation fails and the receiver never receives a complete payload. Store regression covers disable, off/on, revocation, reset and deletion at authorized export. Queued off/on work performs no dial; stopped-engine requests perform no dial, and a real stalled Noise handshake closes in under one second. A saturated loopback TCP backlog verifies cancellation of an actual pending connect without waiting for its dial deadline.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Complete queued-path and multi-process acceptance review; R44 now has bounded paging/deltas and record segmentation, with final scale/resource review still open.

**Acceptance requirement:** Change: gate every outgoing path and every new exchange on the authoritative master setting; propagate cancellation to in-flight work. Verify: disable sync before and during a round, edit dictionary/history, and assert no records leave the device.

## R02 — Outgoing sync does not enforce local peer trust

**Grouped subissues and baseline evidence:**

**Reproduced.** [net.rs](../src/net.rs), `exchange` at line 300 and `sync_now` at line 471. The local `trusted` value is enforced only by the responder. An initiator authenticates the expected identity and accepts the remote verdict, then exports its records even when its own peer row is untrusted. This also leaves a revocation race after a worker snapshots its trusted-peer list. The probe made B trust A while A explicitly distrusted B; A still sent history to B.

- Root cause/correction: Both roles enforce local trust and the captured authorization epoch. Export/apply authorize within their transactions; active policy monitors reject revocation and off/on resurrection.
- Regression coverage and executed validation: Encrypted loopback rejects untrusted peer.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Barrier tests for revocation after dial and immediately before export/apply; atomic policy boundary.

**Acceptance requirement:** Change: require local trust immediately before exporting records in both roles, with revocation semantics for active exchanges. Verify: asymmetric trust and revocation between dial and exchange must reject all content transfer.

## R03 — Deleted transcripts retain their original content and are exported

**Grouped subissues and baseline evidence:**

**Reproduced.** [store.rs](../src/store.rs), `delete_history` at line 443, `clear_history` at line 462, `purge_old_history` at line 614, and `export_history` at line 659. Deletion only sets a flag; both final and raw text remain in the database and in sync payloads, including payloads sent to newly paired devices. Retention is consequently logical hiding, not removal of transcript content.

- Root cause/correction: Delete, clear, expiry, receipt and migration redact payload columns; secure_delete enabled. Schema-8 fragmented incoming history carries its UUID and retention timestamp before publication: clear/reset/expiry preserve identity-only tombstones before discarding pending bytes. Source changes and policy/trust/revocation triggers retire staged snapshots, and deleted/discarded UUIDs acknowledge retirement without restaging raw speech.
- Regression coverage and executed validation: release_regressions::deletion_clear_expiry_and_all_merge_orders_redact (see source for current test names).
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: WAL/checkpoint/backup deletion guarantees and associated audio cleanup still need verification.

**Acceptance requirement:** Change: keep identity-only tombstones and redact payload columns locally and on receipt; account for SQLite WAL/backups when defining deletion guarantees. Verify: delete/clear/expire a transcript, export to a new peer, and inspect both databases for the original text.

## R04 — A stale discarded record resurrects deleted history

**Grouped subissues and baseline evidence:**

**Reproduced.** [store.rs](../src/store.rs), `apply_history` at line 759. The conflict update assigns both incoming flags whenever either incoming flag is true. Thus `(deleted=true, discarded=true)` merged with older `(deleted=false, discarded=true)` becomes visible again. Arrival order changes the final result, violating convergence.

- Root cause/correction: Merge flags with SQL OR; every tombstone redacts text and metrics.
- Regression coverage and executed validation: All six normal/discarded/deleted permutations, repeated three times, converge in disposable databases.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Protocol compatibility review and stale legacy peer behavior.

**Acceptance requirement:** Change: merge monotonic flags with logical OR, or use an explicit versioned state machine; combine this with R03's redaction. Verify: all permutations and repetitions of normal/discarded/deleted snapshots converge and no deleted row returns.

## R05 — Reset retains trusted peers, enables sync, and permits deleted data to return

**Grouped subissues and baseline evidence:**

**Reproduced.** [store.rs](../src/store.rs), `reset_all_data` at line 603; [app_settings.rs](../src/app_settings.rs), reset action around line 4617. Reset physically removes content and settings but leaves `devices` and sync metadata. Default settings turn sync back on. Existing peers can then reintroduce the records whose tombstones were removed. The probe verified preserved trust, enabled sync, and resurrection from a previous history export.

- Root cause/correction: Reset preserves redacted tombstones, revokes peers, resets settings with sync off, advances both sync and capture epochs, and cancels installers before clearing assets.
- Regression coverage and executed validation: Reset/revocation regression and capture_privacy_regressions reject old normal/discarded persistence after reset. Installer reset race regression passes.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Regression follow-up (F12): a private durable intent, transactional pending/completed nonces and an owned background worker coordinate reset/recovery. Failed cleanup keeps capture/sync/installers disabled, reflects the committed settings in GTK, and can be retried without repeating the DB reset or deleting later edits. Native callbacks remain responsive under independent-process DB/installer locks; killed-process recovery and actual older-build refusal of schema 6 pass. Identity/key provisioning is unchanged.
- Unresolved requirements: Active HTTP/WS/CLI disclosure cancellation now has F01 coverage. Pending capture admission, owned warmup/server cleanup, full clipboard privacy boundaries, final filesystem/privacy review and packaged compatibility remain.

**Acceptance requirement:** Change: define reset as a coordinated boundary: stop jobs/sync, clear trust or retain replicated deletion state, then reset settings. Verify: reset a paired device and reconnect it to a stale peer; user data must stay cleared.

## R06 — First-run identity creation is not atomic

**Grouped subissues and baseline evidence:**

**Reproduced.** [device.rs](../src/device.rs), `load_or_create` at line 33. `exists` followed by `fs::write` lets the GUI and service generate different identities against the same file; concurrent readers may see partial contents. One probe run returned four distinct identities for one path. Legacy-key expansion has the same race. Permissions are tightened only after writing, and existing 64-byte keys are not permission-checked.

- Root cause/correction: Cross-process file lock, atomic private key replacement, and existing-key permission enforcement; malformed existing key is preserved as an error.
- Regression coverage and executed validation: Twelve simultaneous creators/migrators agree on persisted identity; malformed preservation and permissions regressions pass.
- Migration/compatibility: Identity is preserved; malformed existing identities are not replaced.
- Unresolved requirements: Fresh-process and kill-during-replacement fault checks still needed.

**Acceptance requirement:** Change: serialize initialization/migration with a lock and atomically install a private file; reread the winning identity. Verify: concurrent first launches all return the persisted identity, including legacy migration and restrictive permissions from creation onward.

## R07 — First-run database-key creation can lose the key to an encrypted database

**Grouped subissues and baseline evidence:**

**Inspected race.** [store.rs](../src/store.rs), `db_key` at line 132; [bin/mywhisper.rs](../src/bin/mywhisper.rs), initialization at line 18. Two processes can both observe `NoEntry`, generate different keys, and overwrite the same keyring entry. Database opening occurs before acquiring the service engine lock. The final keyring secret may differ from the secret used to encrypt the new database. This was not reproduced against a live Secret Service.

- Root cause/correction: Serialize keyring provisioning and DB initialization across processes. Secret Service work has a five-second total deadline and uses encrypted sessions. Locked collections/entries refuse opening without unlock prompts. Provisioning creates a write-once item and verifies its stored secret, never replacing an existing entry. An interrupted request can leave duplicates; existing encrypted data selects its correct key through bounded read-only checks without overwriting secrets or data. Ambiguous new/plaintext provisioning refuses safely. Missing encrypted-database keys require restoration. Startup errors are reported. A narrow authenticated Secret Service 5.2.0 source patch makes the no-prompt decision directly at CreateItem; reply interception proved racy in the parallel regression and was removed.
- Regression coverage and executed validation: The full parallel Rust suite passes the actual-library private-D-Bus regression asserting CreateItem with replace=false, delayed Dismiss acknowledgement and zero Prompt calls; the original API still prompts as expected. Duplicate-candidate unit tests assert unchanged database bytes and recovery in either candidate order. The current `scripts/keyring-smoke.sh` passes with an isolated real Secret Service: eight independent concurrent creators share identity/key, missing-key refusal/restoration, duplicate-key recovery without replacement, locked refusal under two seconds, private-daemon restart recovery, and two five-second stalled-daemon attempts with unchanged database bytes followed by recovery. The patch archive checksum and every original/copied file digest were authenticated against the registry archive; release inventory preserves both original licenses and validates complete local source provenance. Packaging regressions reject altered/missing files and unlisted modifications.
- Migration/compatibility: Existing service=mywhisper/username=state-db entries remain compatible. Background opening requires an unlocked default login collection and never launches a prompt; errors explain how to unlock/configure it and retry. Legacy duplicate keys are preserved and automatically selected only when existing encrypted data proves the winner. No project LICENSE added. Interrupted database migration and other Secret Service implementations still need review.
- Unresolved requirements: Actual killed-process/delayed CreateItem fault injection, interrupted migration recovery, other supported keyring implementations and user-visible startup recovery behavior still need acceptance coverage.

**Acceptance requirement:** Change: serialize key provisioning and database initialization across processes; preserve recoverability instead of treating all startup errors as fatal panics. Verify: simultaneous first GUI/service launches against a disposable Secret Service, followed by fresh-process reopening.

## R08 — Release/cancel can arrive before the queued press installs its session

**Grouped subissues and baseline evidence:**

**Reproduced.** [bin/mywhisper.rs](../src/bin/mywhisper.rs), Show handler around line 248; [transcribe.rs](../src/transcribe.rs), `begin_utterance` at line 2301 and `end_utterance_inner` at line 2693. Press is queued in the shared pool while release runs immediately. If press is delayed, release finds no session; the queued job then starts a recording that has already been released. Streaming connection setup also happens before publishing the live session, creating another lost-release interval. The probe occupied the four workers, queued press, released, and observed `has_live()==true` afterward.

- Root cause/correction: Reserve generation at input edge before queued setup. Release/cancel supersedes queued press; streaming setup runs as an owned session task after reservation.
- Regression coverage and executed validation: Code inspected; dedicated queued-press/setup regression not yet added.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Capture still starts after queued settings/setup work; synchronous capture-edge ownership and fake stalled upgrade tests.

**Acceptance requirement:** Change: represent press/release/cancel as one generation-owned state machine before scheduling setup. Verify: fast taps, saturated workers, slow WS handshake, and cancel during setup never leave a live tap or mic.

## R09 — An old release disarms the next recorder

**Grouped subissues and baseline evidence:**

**Reproduced.** [transcribe.rs](../src/transcribe.rs), release-settle thread around line 2729; [audio.rs](../src/audio.rs), `set_recorder` at line 102. After waiting for the old recorder, the tail thread unconditionally clears the process-global recorder slot. A new press during that interval installs a new recorder which the old release then detaches. The replacement-recorder probe confirmed samples stop reaching it.

- Root cause/correction: Recorder detachment compares Arc ownership; old settle/cancel cannot clear a different recorder.
- Regression coverage and executed validation: Code inspected; explicit rapid replacement regression pending.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Rapid press/release/press with configured delay and fake microphone.

**Acceptance requirement:** Change: compare ownership/generation before clearing the global tap. Verify: repeated press/release/press at intervals shorter than the flush delay, including configured release delay, preserves the new recording.

## R10 — Canceling speculative recordings permanently occupies workers

**Grouped subissues and baseline evidence:**

**Reproduced.** [transcribe.rs](../src/transcribe.rs), speculative setup around line 2360, `run_spec` at line 2149, and `cancel_utterance` at line 2586. Speculation has its own stop flag; cancellation, stale-session replacement, and overlapping-session drop never set it. Dropping the result receiver does not stop the loop. Four canceled speculative recordings consumed all four workers; a subsequent trivial job never ran.

- Root cause/correction: Session destructor sets speculative/pump cancellation and settle flags. Dedicated session tasks are budgeted and transferred to an owned joining reaper.
- Regression coverage and executed validation: Repeated speculative cancellation preserves settled tail and completes without waiting for the ten-second tick.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Hundreds of cancellation/replacement cycles, worker sentinel/resource counts and cancellation during inference.

**Acceptance requirement:** Change: make session destruction cancel and join all owned tasks, and move long-running loops out of the short-job pool. Verify: hundreds of cancellations and replacements leave a fixed worker count and keep a sentinel task responsive.

## R11 — Unicode dictionary prompts panic; a panic can leave dictation permanently busy

**Grouped subissues and baseline evidence:**

**Reproduced helper panic; inspected busy-state consequence.** [textnorm.rs](../src/textnorm.rs), `dictionary_prompt` at line 28, slices UTF-8 at an arbitrary byte budget. A long CJK word with the 890-byte budget panics. This helper is reachable while finalizing a file transcription. [transcribe.rs](../src/transcribe.rs), worker around line 2750, resets `BUSY` and invokes completion only after finalization returns; the outer runtime catches panics without running those steps.

- Root cause/correction: Dictionary budget truncates at a UTF-8 boundary. RAII finalization guard clears busy and completes even on unwind.
- Regression coverage and executed validation: Every byte budget 0–901 over CJK/emoji/accented input passes.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Inject finalization/completion panic and verify next session; enforce each actual provider budget.

**Acceptance requirement:** Change: truncate on character boundaries, enforce the actual provider budget, and use a finalization guard that always clears busy/completes. Verify: CJK/emoji/accented inputs crossing each budget, plus injected worker panics, allow the next utterance.

## R12 — An unauthenticated discovered device ID can panic the GTK UI

**Grouped subissues and baseline evidence:**

**Reproduced UI panic with injected discovery data.** [net.rs](../src/net.rs), mDNS resolution around line 680, accepts any nonempty TXT node ID. [app_sync.rs](../src/app_sync.rs), `short_id` at line 50, slices it at byte 8 and at `len-4`. A sufficiently long non-ASCII ID panics; arrival through the refresh timer would run inside a GTK callback, where unwinding can abort the process. Gossip can introduce malformed IDs too.

- Root cause/correction: Canonical lowercase hex IDs are checked at discovery/contact/gossip boundaries; display shortening uses characters.
- Regression coverage and executed validation: Malformed gossip and Unicode short-ID helper checks pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Native device-list injection regression and validation of every manual/storage boundary.

**Acceptance requirement:** Change: validate canonical 64-character hexadecimal IDs at all boundaries and make display shortening UTF-8 safe. Verify: malformed IDs delivered through discovery and gossip cannot enter storage or crash a rendered device list.

## R13 — WebSocket setup and TLS streaming can block indefinitely

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `ws_connect_url` at line 1237, `ws_read_timeout` at line 1316, and finalization at line 2828. Socket timeouts are installed after `tungstenite::connect`, so TCP/TLS/HTTP upgrade has no application deadline. Both timeout helpers cover only plain streams; WSS has neither the short partial-read timeout nor the final-read timeout. A quiet/stalled server can pin a pump, then finalization waits on unbounded `recv()`, leaving Processing and the busy flag active.

- Root cause/correction: TCP/TLS/upgrade deadline installed before negotiation; timeout helpers cover Rustls; bounded pump result wait and total final-reply deadline.
- Regression coverage and executed validation: Code compiles; authenticated WSS/stalled-upgrade acceptance tests pending.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Bound DNS; propagate cancellation through connect/TLS/read/write and test real authenticated WSS.

**Acceptance requirement:** Change: bound connect/upgrade/read/write/finalization for plain and TLS transports, with cancellation. Verify: servers that accept but never upgrade, never emit partials/finals, or stop consuming data recover within documented deadlines.

## R14 — Multichannel CPAL input is treated as mono

**Grouped subissues and baseline evidence:**

**Inspected.** [mic.rs](../src/mic.rs), `CpalMic::build_on` around line 267, uses the device's default channel count but feeds every interleaved sample to a mono recorder at the original frame rate. Stereo therefore becomes twice as many mono samples with altered timing and signal; all sample-format branches have this problem. It affects the fallback capture path used when `parec` is unavailable.

- Root cause/correction: All three supported CPAL formats downmix complete interleaved channel frames to mono at the original frame rate. Partial frames carry across callbacks and reset at capture ownership changes; nonfinite input and unsupported callback/channel/rate bounds fail explicitly.
- Regression coverage and executed validation: Existing format assertions remain and pass, alongside partial-frame/malformed-input checks. `scripts/cpal-smoke.sh` passes nine actual CPAL/ALSA file/null PCM combinations (F32/I16/U16, 1/2/6 channels, 48 kHz), asserting sample values, rate, progress, health and cleanup. The virtual source is documented by [ALSA](https://www.alsa-project.org/alsa-doc/alsa-lib/pcm_plugins.html); it reads only a private synthetic FIFO and opens no physical microphone.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Physical device formats/device changes and final integration reruns; native software-device gates supplement these requirements.

**Acceptance requirement:** Change: negotiate mono or downmix complete frames before metering/capture. Verify: known mono/stereo/multichannel fixtures preserve duration, pitch, channel alignment, and recognition input format.

## R15 — Server port and exposure edits do not invalidate running-server identity

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), server keys at lines 919, 1117, and 1186; reuse at line 736. Cache keys omit port and bind host. A live matching child returns success without moving it. Changing port makes clients dial a different endpoint; turning LAN exposure off can leave the original externally bound whisper server running.

- Root cause/correction: Supervised server identity includes bind host and port. Configuration changes cannot reuse an incompatible child.
- Regression coverage and executed validation: Code inspected; listening-socket regression pending.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Live warm server port/exposure changes and successful subsequent inference.

**Acceptance requirement:** Change: include all launch-relevant parameters in the supervised configuration and restart/rebind explicitly. Verify: edit port and exposure while warm, verify actual listening sockets, and successfully transcribe afterward.

## R16 — Default sync listener has no connection/concurrency budget

**Grouped subissues and baseline evidence:**

**Inspected resource-exhaustion paths.** [net.rs](../src/net.rs), `serve` at line 525, creates a new OS thread for every accepted connection. A Noise session does not require a previously trusted identity, and `recv_msg` at line 257 permits messages up to 256 MiB before the signed hello/trust check. Empty decrypted chunks can make no progress indefinitely; socket reads have per-call timeouts but no whole-message deadline, and writes have no timeout. Sync is enabled by default and binds all interfaces.

- Root cause/correction: Listener caps owned accepted workers at eight, bounds pre-authentication/transport messages, rejects empty/overlong chunks. Frame IO and handshake have absolute deadlines.
- Regression coverage and executed validation: Loopback malformed lengths and slow trickle frame regression pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Bounded connection load tests, whole exchange byte limits, thread/memory measurements and dictation responsiveness.

**Acceptance requirement:** Change: cap active/pending connections, use small pre-authentication message limits, reject empty chunks, and enforce byte/progress/total deadlines plus write deadlines. Verify: bounded local load and slow-peer tests preserve dictation and a fixed memory/thread budget. No denial-of-service attack was performed during this audit.

## R17 — Raw and final transcripts are written outside the encrypted history store

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), partial/segment logs around line 1402, speculative logs around line 2199, and `deliver` at lines 3353 and 3387. `eprintln!` records speech before checking history/privacy settings. In the systemd service it goes to the journal. The five-file tracing rotation in [paths.rs](../src/paths.rs) does not control these stderr logs. Disabling or clearing history consequently leaves another retained transcript copy.

- Root cause/correction: Speech-bearing raw/final/partial/segment/speculative and readiness-reply logs removed; runtime stdout/stderr disabled. Whistle errors omit stderr content. Plan descriptions/Debug omit endpoint/key/model/path fields. HTTP diagnostics classify failures and numeric status without credential-bearing URLs or reply content; all provider/cleanup bodies have a 1 MiB limit and strict complete UTF-8 decoding. Speech logging is not offered as an application diagnostic feature; encrypted history remains the explicit transcript viewing feature. Typed JSON errors from preserved settings, encrypted sync payloads and provider contracts report category/line/column without echoing offending values.
- Regression coverage and executed validation: A disposable child drives real loopback HTTP cleanup and the delivery/persistence path with history/audio/clipboard disabled; captured stdout/stderr contains neither synthetic raw/final speech, endpoint/query secrets, tokens nor provider error body. Oversized declared/chunked responses, partial transfers, invalid UTF-8 and content-bearing HTTP errors are rejected with safe diagnostics. The expanded captured-child regression reproduced a malformed-settings/peer value leak before correction, then passed after correction while asserting safe-mode settings, original JSON preservation and encrypted-loopback rejection. These regressions pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Full service journal/stderr scan with synthetic speech across actual advertised engines and remaining non-HTTP diagnostic paths; active HTTP cancellation/progress work remains under lifecycle/provider findings.

**Acceptance requirement:** Change: redact speech by default and make content logging an explicit, bounded diagnostic mode. Verify: with history disabled, capture the service journal/stderr and runtime logs while dictating synthetic secrets; none should contain speech.

## R18 — Process adoption/reaping can target unrelated applications

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `reap_stale_servers` at line 473, `find_server_pids` around line 523, and `ensure_server` at line 696. Ownership is inferred from substring matches anywhere in `/proc/<pid>/cmdline`; adoption is not tied to the process actually listening on the selected port. A hand-started server or another program with a matching argument may be adopted or terminated. PID-file checks also accept the broad `sherpa-onnx` substring. The supposedly unconditional untracked sweep returns early when the PID map is empty.

- Root cause/correction: Removed substring sweeps and adoption. Only owned Child handles are stopped; occupied port returns explicit configuration error. Private atomic status metadata records the ready child PID, process start time and port. Cross-process status verifies start time and actual listening-socket ownership; stale/legacy PID-only records cannot report managed status. Status metadata never grants signalling authority.
- Regression coverage and executed validation: An actual unrelated Python TCP listener with whisper-server/sherpa-onnx command-line marker arguments remains alive after stale PID-file cleanup, rejected local server setup, and supervisor shutdown. Regressions also assert that status matches the actual socket, rejects wrong/reused start time and wrong port, and stops reporting a closed listener. These pass in the full parallel 164-test Rust suite and source-built Nix test phase.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Final long-running supervised restart/status and real hand-started advertised runtime coexistence checks remain; the synthetic native listener establishes that marker arguments and stale PIDs do not authorize adoption or termination.

**Acceptance requirement:** Change/verify: supervise only verifiable children, record executable/start-time/configuration, associate adopted sockets with their process, and test alongside an unrelated same-user whisper server and commands containing the marker.

## R19 — LWW comparisons are not atomic with writes

**Grouped subissues and baseline evidence:**

**Inspected race.** [store.rs](../src/store.rs), `apply_words` at line 687 and `apply_snippets` at line 718, read a version and later unconditionally upsert. Multiple connections can both pass the comparison, then the older writer overwrites the newer one. Sync can run concurrently from several independent threads.

- Root cause/correction: LWW predicate is part of SQL ON CONFLICT; remote cross-table merge uses one transaction.
- Regression coverage and executed validation: Concurrent connections and rollback/idempotence regressions pass.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Deterministic local-edit/two-peer barrier coverage and remaining gossip/contact transaction boundary.

**Acceptance requirement:** Change/verify: put the version predicate in the SQL conflict update or a transaction with correct contention handling. Exercise simultaneous local edit and two peer merges with barriers; the maximum version must always survive.

## R20 — Wall-clock LWW loses fast local edits and traps future-dated data

**Grouped subissues and baseline evidence:**

**Reproduced.** [store.rs](../src/store.rs), `now_ms` at line 23 and local CRUD at lines 354–411. Strict tuple comparison ignores a second operation in the same millisecond from the same node. A future-dated peer record also defeats a local deletion/update until the clock catches up. The probe demonstrated both silent no-op deletion cases.

- Root cause/correction: Local logical clock advances past stored versions; reject negative/extreme peer timestamps.
- Regression coverage and executed validation: One hundred fast edits, future-dated peer day, local deletion and invalid maximum timestamp checks pass.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Clock rollback/overflow recovery and protocol clock validation across every input.

**Acceptance requirement:** Change/verify: use a monotonic logical/hybrid clock that advances past stored versions for local edits; validate extreme peer times. Test equal-millisecond updates, clock rollback, and a peer one day ahead.

## R21 — Invalid or older settings JSON silently resets every preference

**Grouped subissues and baseline evidence:**

**Inspected.** [store.rs](../src/store.rs), `load_settings` at line 293; [settings.rs](../src/settings.rs), `AppSettings` at line 10. Deserialization errors are discarded and replaced with the complete default object. Several sections and fields are required. A missing field, unknown enum, or damaged blob can reset API configuration and privacy switches, including enabling sync; the next ordinary save overwrites the prior blob.

- Root cause/correction: Compatible fields default individually; corrupt settings blob preserved, load fails closed for sync/history/audio and save refuses overwrite.
- Regression coverage and executed validation: Minimal/partial configurations and corrupt preservation checks pass.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Versioned settings migration, unknown enums, visible recovery message and privacy defaults for absent fields.

**Acceptance requirement:** Change/verify: version and migrate settings, default compatible missing fields individually, surface errors, and preserve the original blob. Test historical/minimal configurations and invalid enums without changing unrelated privacy settings.

## R22 — Database migrations lack a coordinated, recoverable boundary

**Grouped subissues and baseline evidence:**

**Design risk.** [store.rs](../src/store.rs), plaintext migration at line 197 and `open` at line 216. Migration uses a shared `.migrating` path and rename without a cross-process lock or backup/recovery protocol. GUI and service can enter it together. The schema sets `user_version=2` before best-effort alterations, ignores alteration errors, and does not reject newer schemas. Wrong encryption keys are also routed through the plaintext-detection path. Existing WAL/active connections need explicit migration tests; data corruption was not demonstrated here.

- Root cause/correction: DB initialization lock, private unique staging, atomic plaintext replacement, transactional schema upgrade and newer-schema rejection. Wrong key is distinguished from plaintext.
- Regression coverage and executed validation: Eight simultaneous migrators, wrong-key preservation and newer-schema tests pass.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Active WAL, disk-full, interrupted migration/recovery, pre-migration future-version rejection and directory durability.

**Acceptance requirement:** Change/verify: exclusive versioned transactions, distinguish wrong key/plaintext/corruption, preserve a recoverable backup, and fault-test two simultaneous openers, active WAL, disk-full, interrupted migration, and newer schemas.

## R23 — Hotkey cancellation/muting/reconfiguration loses session state

**Grouped subissues and baseline evidence:**

**Inspected.** [hotkey.rs](../src/hotkey.rs), event polling at line 387 and dictation callback at line 654. Escape calls cancellation without clearing the shared toggle or smart-press state, so the next Toggle press can stop a nonexistent recording. Disabling shortcuts skips releases and held-key updates as well as presses. Restarting the listener discards active bindings without cancel/release. A recording may stay armed after mute/reconfiguration; Smart/Toggle have no Push watchdog protection.

- Root cause/correction: An activation records the binding and monotonic press that opened it. Push/Smart releases cannot retire another binding; Toggle/Smart presses deliberately close the active session. Cancel retires ownership, Escape clears watched edges, muted events still update held keys, and the mute transition cancels even a queued press. Listener replacement joins its predecessor before starting another.
- Regression coverage and executed validation: Meaningful regressions pass for cancellation/retry in all modes, mixed Push/Toggle/Smart ownership, stale Smart releases, backward timing, Escape, muted/repeated input and 20 rapid press/release cycles. Existing real uinput Down/Up test continues to pass locally.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Full service-level mixed-mode/reconfiguration and queued capture acceptance still require live verification; latest key dispatch and device dependency changes need final reruns.

**Acceptance requirement:** Change/verify: centralize activation state with session ownership; process releases while muted and cancel active bindings on restart. Test Escape→Toggle, mute while held, and edit bindings during each activation mode.

## R24 — Shortcut capture does not suspend the service listener

**Grouped subissues and baseline evidence:**

**Inspected.** [app_settings.rs](../src/app_settings.rs), persistence/capture paths at lines 820, 1204, 1233, and 1271; [hotkey.rs](../src/hotkey.rs), `apply_from_settings` at line 710. These start a second evdev listener in the GUI with log-only hooks. `hotkey::stop()` affects that process only. The service continues listening while the user records a shortcut, so testing an existing binding can start real dictation. This contradicts the frontend-only ownership model.

- Root cause/correction: The GUI creates no engine listener. It acquires a nonblocking capture lock and creates a fresh random request nonce. An engine thread queues suspension after stopping key admission; its GTK handler retires capture, closes the microphone and acknowledges only a still-current request. The GUI swallows keys until that exact acknowledgement, bounds waiting, and releases suspension on completion, cancel, page unmap and drop. Without an engine owner, the lock still prevents a concurrent engine startup from admitting keys. The listener retains its last handled nonce across lock release: reacquisition initially exposes the old request file, which must not replay suspension and consume the new acknowledgement.
- Regression coverage and executed validation: A full-suite rerun exposed an old-nonce replay race. A forced startup-gap reproduction fails against d6fbbd8, then passes with the correction; twenty repeated owned two-process handshakes pass without deadline increases. The stale-ack fixture now uses a publication barrier instead of a scheduling-dependent sleep. The full suite passes 191 tests, strict Clippy/formatting/release build pass, both GTK configurations pass, and seven real uinput checks pass. Real two-process file-lock/nonce regression passes: stale acknowledgement rejected, fresh request accepted, no-binding engine acknowledges without opening hardware, and process termination releases the engine lock. During the later paging checks, unrelated parallel fork/pre_exec tests intermittently inherited the parent test’s flock until exec, racing its immediate release assertion. The disposable fork_lock_probe.py demonstrates that kernel lifetime. The entire lock owner now runs in a fresh process, preserving every original freshness, recovery and immediate-release assertion. Twelve repetitions and the parallel full suite pass; no listener deadlines or production behavior changed. Native GTK checks pass waiting-key suppression, stale/current nonce behavior, completed F14 capture and page-unmap recovery in both configurations.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Actual microphone/hotkey service capture with supported input devices still needs a live check. GTK and process tests cover ownership/protocol. The required native companion passes descriptor preservation and exact Down/Up ordering through real uinput; actual service/microphone capture suspension is still a separate gate.

**Acceptance requirement:** Change/verify: keep engine listeners in the service and send an explicit capture/suspend request to it. Recording and canceling shortcuts must leave no service dictation or extra GUI evdev listener.

## R25 — Legacy custom shortcuts migrate to F13

**Grouped subissues and baseline evidence:**

**Reproduced.** [settings.rs](../src/settings.rs), `HotkeySettings` at line 280 and `migrate_legacy` at line 339. Missing `dictation_hotkeys` deserializes to the nonempty default F13 list. That list takes precedence over the legacy custom slot, then migration clears the custom value. The probe's legacy `Ctrl+D` becomes F13.

- Root cause/correction: Custom deserializer distinguishes absent new binding list, explicit empty list and legacy custom slot.
- Regression coverage and executed validation: Legacy Ctrl+D, default F13, explicit F14 and explicit empty fixture checks pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Startup persistence/migration validation and user-facing empty-list policy.

**Acceptance requirement:** Change/verify: distinguish an absent new list from an explicitly configured list and prefer the legacy value only during migration. Add fixtures for old custom shortcuts, default shortcuts, explicit new lists, and empty lists.

## R26 — Shortcut parsing, conflict detection, and inverse key mapping disagree

**Grouped subissues and baseline evidence:**

**Inspected.** [hotkey.rs](../src/hotkey.rs), `token_for_evdev_code` at line 162, `parse_combo` at line 220, and modifier matching. Multiple target keys overwrite the prior target instead of rejecting a combo such as `Ctrl+A+B`. Matching accepts extra modifiers, so nested combos can fire together although exact-equality conflict checks accept them. The inverse map labels evdev 54 (Right Shift) as C and emits punctuation tokens the parser rejects; many letter mappings fall back to layout-derived keyvals.

- Root cause/correction: One canonical physical-key table serves parsing, inverse capture and conflict checks, including explicit punctuation names/symbol aliases. Reject multiple targets and empty/malformed components; bound input. Exact initial modifier matching avoids nested activation. Recorded bindings use the same normalized conflict check as typed bindings, and the UI explains physical-key behavior across layout changes.
- Regression coverage and executed validation: Exhaustive canonical/inverse roundtrip, every punctuation alias, malformed/Unicode boundaries, multi-target and exact modifier regressions pass. Native synthetic ñ-keyval/semicolon-position regression added; live layout validation remains separate.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Actual supported non-US layout/device capture and conflict feedback integration remain to be verified.

**Acceptance requirement:** Change/verify: one canonical key map, exactly one target, explicit modifier-match policy, and overlap-aware conflict checking. Exhaustively round-trip supported keycodes and test nested bindings/non-US layouts.

## R27 — Held keys are merged across input devices and survive disconnects

**Grouped subissues and baseline evidence:**

**Inspected.** [hotkey.rs](../src/hotkey.rs), `poll_events` at line 387 and device rescan logic. A single global set represents keys held on several keyboards. A release on one keyboard can clear a modifier still held on another; a vanished device does not remove only its own held keys. This can produce missed/incorrect shortcuts and stuck modifier state.

- Root cause/correction: Per-device sets form the aggregate; a target releases only after its last holder releases. Each watched binding remembers its press device, and disconnect cancellation carries that binding identity. Removing an unrelated device cannot cancel another binding/session; losing the last required modifier cancels its affected binding.
- Regression coverage and executed validation: Two-device aggregate and overlapping target/modifier regressions pass; all-mode binding-specific unplug checks pass. Last-owner modifier unplug regression added. Native descriptor and exact Down/Up assertions pass via `scripts/native-hotkey-smoke.sh`; every emitted event is confined to an exclusively owned uinput device. The default unit checks exercise the same driver with deterministic frames and retain buffered descriptors across rescans, so package sandbox checks need no physical keyboard.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Two actual virtual keyboard overlapping modifiers and disconnect during every mode.

**Acceptance requirement:** Change/verify: track held keys per device, aggregate counts, and release owned state on disconnect. Use two virtual keyboards with overlapping modifiers and unplug a device during an active binding.

## R28 — Capture and inference failures lack actionable user feedback

**Grouped subissues and baseline evidence:**

**Inspected.** [mic.rs](../src/mic.rs), `ParecMic::build` at line 32 and `MicInput`; [transcribe.rs](../src/transcribe.rs), empty capture at line 2870; [bin/mywhisper.rs](../src/bin/mywhisper.rs), completion handling. A `parec` child that immediately exits is still accepted as an active mic; its liveness is not monitored. Fallback simulated bars can look like listening. Empty audio returns `Ok(None)` and calls `engine_ok()`, while errors mostly go to stderr and the pill hides. `OverlayState::Error` is not wired to these failures.

- Root cause/correction: Parec uses owned process groups, nonblocking sample IO with byte carry, two-second audio readiness, monitored health, stop/join and reap. CPAL errors update health. Microphone setup runs on an owned worker; real-source failure no longer produces simulated bars.
- Regression coverage and executed validation: Four lifecycle regressions pass: exited/silent startup, repeated cancellation cleanup, partial samples/device exit, and real failure versus explicit simulation.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Wire capture/provider/delivery errors into visible recovery UI; ensure warm device changes, capture ownership at input edges, and actual device acceptance.

**Acceptance requirement:** Change/verify: represent real capture availability separately from visual simulation, monitor child/stream health, and surface failed setup/capture/transcription/paste with recovery actions. Test absent mic, invalid source, exited `parec`, missing runtime, and refused server.

## R29 — Whistle FIFO/process finalization has unbounded and incomplete paths

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), Whistle CLI around line 1494, stream around line 1630, `feed` around line 1695, and `finish` around line 1745. Checking child liveness does not guarantee it has opened the FIFO; blocking write-only open can hang forever. Writes and the file CLI lack deadlines. The stdout reader occupies a pool worker and is not joined before taking its last text. Timeout/nonzero exit with partial text is treated as success, suppressing fallback. Drop kills but does not wait, and these children lack server-style parent-death handling.

- Root cause/correction: Whistle owns its process group, FIFO, private staging and output. Nonblocking startup and writes have deadlines; output drains before success; nonzero/timeout is rejected and Drop kills/reaps. The actual pinned wire has newly committed text/word deltas and an uncommitted pending guess. The parser now validates and appends deltas, excludes pending, retains prior text on an empty terminal delta, and caps the aggregate transcript.
- Regression coverage and executed validation: Alive-never-open, blocked input, final line at exit, nonzero partial result, private staging and IO-limit checks pass. F01 proves active file/FIFO cancellation and reset, child reaping and staging cleanup. The new before-fix regression loses the initial committed delta. Unicode, malformed JSON/word/timestamp, aggregate-limit and actual FIFO-EOF regressions now pass. A real verified model/runtime check confirms all three upstream committed deltas equal the frontend result in order. Full Rust suite: 190 passed; formatting, strict Clippy, release build and both native UI configurations pass. Known synthetic fixtures pass unchanged expected-word assertions for real Needle file/FIFO and Nemotron CPU streaming. Evidence: synthetic-needle-contract.json and validation-review-needle.json.
- Migration/compatibility: The legacy text-only cumulative format remains supported. Pinned Needle events carrying words are accumulated as committed deltas. No persistent schema or user-data migration.
- Unresolved requirements: Parent-death cleanup, larger recordings and final compatibility/resource review remain. The original Hello fixture also exposes upstream model accuracy errors: Needle commits “The long” and Nemotron recognizes “Hell”. Those failures remain recorded; the correction preserves actual committed words and does not manufacture a missing recognized word.

**Acceptance requirement:** Change/verify: nonblocking/deadline-aware FIFO startup and IO, cancellable child supervision, drain/join output before success, and distinguish partial results. Test alive-but-never-open, blocked reader, final line at exit, crash, and service death.

## R30 — Speculative finalization can drop unprocessed tail audio

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `finish_session` around line 2868 calls `Recorder::finish()` before stopping/joining the speculative worker. `finish()` takes the recorder's samples. The worker's final snapshot is then empty, so samples received since its previous tick do not reach its buffer. The final decode uses that incomplete buffer instead of the full `pcm` already taken.

- Root cause/correction: Stop speculation and settle before taking complete recorder buffer; final decode uses the full buffer instead of speculative partial text.
- Regression coverage and executed validation: Repeated speculative cancellation/tail-buffer check passes.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Deterministic final inference test with speech arriving only after the last tick.

**Acceptance requirement:** Change/verify: stop the speculative loop and provide the complete settled buffer before consuming it; retain a full-decode fallback. Feed speech only between the final speculative tick and release and assert it appears in the final decode input.

## R31 — Speculative agreement confuses rolling-window and full-prefix text

**Grouped subissues and baseline evidence:**

**Inspected algorithm defect.** [transcribe.rs](../src/transcribe.rs), `run_spec` around line 2188; [speculative.rs](../src/speculative.rs). Each hypothesis is for a moving unconfirmed window, but confirmed window text replaces the entire committed string and its word count is compared with the entire previous commitment. Window shifts invalidate that comparison and can stall/drop prior commitments. The commit sample index is a fixed audio heuristic rather than a word boundary. The documented full-decode fallback on failed merge is not implemented.

- Root cause/correction: Hypotheses now use a common full-prefix origin; uncertain speculative text is not used as authoritative final text; full decode retained.
- Regression coverage and executed validation: Current speculative unit coverage passes.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Deterministic multi-window punctuation/no-overlap/long speech tests; explicit supported speculative optimization behavior and performance.

**Acceptance requirement:** Change/verify: track text/audio origins for each window, append newly confirmed content, and fall back to full decoding when agreement is uncertain. Test deterministic multi-window hypotheses with punctuation changes, long speech, and no overlap.

## R32 — Delivery is outside the busy guard and can overlap or reorder

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), worker around line 2764 clears `BUSY` before `deliver`, which performs potentially slow LLM cleanup and clipboard operations. A later utterance can finish/paste first. Concurrent clipboard save/copy/restore sequences can overwrite each other's output or the user's new clipboard. Duration is measured at finalization, including flush/queue delay, while its start is recorded after session setup; usage statistics therefore do not consistently represent speaking time.

- Root cause/correction: A bounded FIFO finalization worker owns cleanup, persistence and delivery through completion. Capture privacy/reset epochs also invalidate queued persistence.
- Regression coverage and executed validation: Ordered runtime regression holds the first job while later jobs queue; panic does not break FIFO worker. Completion-guard failure recovery passes.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Capture-edge timestamps, integrated delayed cleanup ordering and clipboard selection ownership/consumption remain.

**Acceptance requirement:** Change/verify: ordered delivery with its own ownership/queue and capture-edge duration timestamps. Force the first cleanup to be slower than the second and check paste/history order and clipboard state.

## R33 — Capture memory and streaming copying are unbounded

**Grouped subissues and baseline evidence:**

**Inspected.** [audio.rs](../src/audio.rs), `Recorder::push` at line 41 and `snapshot` at line 57; streaming/speculative pumps in [transcribe.rs](../src/transcribe.rs). All samples and slice stamps remain in RAM for the entire utterance, even after sending chunks. Frequent snapshots clone the entire growing buffer, producing quadratic total copying. Toggle/Smart recording can continue indefinitely. CPAL callbacks allocate and contend on these mutexes on the real-time audio thread.

- Root cause/correction: Recorder enforces ten-minute/64 MiB sample and 65,536 slice limits, ignores empty slices and refuses rate changes/overflow. Streaming/Whistle/speculative consumers copy incremental samples. CPAL uses a preallocated 32-entry lock-free queue and 512-frame batches across callbacks, with an owned drain worker; callback paths neither allocate capture buffers nor wait on recorder locks. Every packet retains its original recorder. Ownership changes and shutdown flush partial batches; settled recorders reject late packets. Saturation/invalid capture gets explicit error feedback and is checked before all finalization paths.
- Regression coverage and executed validation: A ten-minute 44.1 kHz synthetic capture verifies fixed allocation bounds and exactly linear streaming copying. Thread-local allocation tracking verifies zero allocations during callback conversion/queueing while the recorder mutex is held. Regressions cover one-sample callbacks, cross-session ownership, partial shutdown batches, queue saturation, empty-slice metadata bounds and rate changes. All pass, plus nine native CPAL/ALSA format/channel gates. The native gate failed before batching because tiny callbacks filled packet slots; batching corrects this without increasing queue size/workers or weakening the health assertion.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Longer complete-engine slow-inference/cancellation memory/latency measurements and physical device acceptance; audit the remaining callback admission lock and real device timestamp behavior under repeated ownership transitions.

**Acceptance requirement:** Change/verify: explicit duration/size limits, incremental/ring-buffer reads with a bounded fallback policy, and minimal callback work. Benchmark long synthetic recordings and cancellation under slow inference with memory/latency limits.

## R34 — Server readiness and keep-warm ownership are insufficient

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `ensure_server` at line 696 and reaper around line 631. A child is placed in the shared map before readiness/probe completes; concurrent ensure calls return success based on liveness/key alone. This lets inference race startup probing. Last-use timestamps are refreshed by ensure, with no active-session lease, so a long recording can outlive the keep-warm timeout and lose its server. Failed startup leaves a live cached entry eligible for reuse.

- Root cause/correction: Serializes readiness with startup guard cleanup and active-session keep-warm leases. All supervised servers now own unreaped leaders/process groups with parent-death race protection; replacing/failing/stopping drops and reaps that exact owner, never a persisted numeric PID.
- Regression coverage and executed validation: Actual owned Whisper CPU/Vulkan, Sherpa offline Parakeet CPU, and Sherpa online Nemotron CPU servers downloaded, initialized, decoded synthetic speech and stopped successfully. CUDA correctly exposes missing libcudart.so.12/libcublas.so.12 prerequisites.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Concurrent first starts, all failed probes, slow readiness and a session longer than keep-warm still need deterministic checks; CUDA execution and packaged NixOS runtime loader compatibility remain.

**Acceptance requirement:** Change/verify: explicit Starting/Ready/Failed states and session leases; all callers await the same readiness result. Test concurrent cold start, failed probe, and recording longer than the smallest timeout.

## R35 — Cohere is selectable/downloadable but cannot be used

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `local_plan` at line 351, always rejects Cohere. The registry and UI allow selecting/downloading its large model, and CHANGELOG language support suggests it is operational. Even a fully downloaded configuration cannot create a plan.

- Root cause/correction: The baseline unconditionally rejects Cohere. Rechecking both pinned upstream source and the authenticated distributed CPU binary proves the previous library-only explanation incorrect: encoder, decoder and language flags exist. Cohere now creates an offline plan after verifying all four required model files and one of its fourteen explicit language codes. Server launch uses the dedicated Cohere encoder/decoder flags, without transducer/joiner arguments or silent English fallback. Selection and verified download are enabled; Settings explains the explicit-language requirement and excludes automatic detection for local Cohere while preserving existing unsupported values for correction.
- Regression coverage and executed validation: Actual authenticated sherpa-cpu archive SHA-256 d0f96c8b65c6cd0974fada22737e337de81bc8cd2abbec2e39caf358b1eec5fc exposes all three required flags with successful --help. Corrected plan regression covers all fourteen codes, missing external encoder data and malformed/automatic/unsupported language refusal. Cancelled download coverage preserves existing model files. The new native UI check reproduced an incorrectly offered fifteenth auto option before correction; both configurations then passed with G_DEBUG=fatal-criticals, asserting selectable Cohere, available download, fourteen explicit languages, preserved Spanish and visible language guidance. Real pinned-model CPU inference passed in 263.7 seconds including verified 1,699,791,751-byte archive download and installation: the production plan, dedicated flags, real readiness inference and synthetic speech decode succeed, then the owned server is stopped and all disposable resources are removed.
- Migration/compatibility: Existing downloaded Cohere files and saved provider/language values remain intact. The false unsupported gate was removed after concrete runtime-contract evidence. Existing auto/unknown codes remain visible and editable; plan creation reports the required explicit supported language. Remote fallback retains its language behavior.
- Unresolved requirements: Cohere CUDA and further spoken-language/backend integration checks, followed by final compatibility/resource cleanup review. This finding remains open for those obligations; CPU inference is now demonstrated.

**Acceptance requirement:** Change/verify: implement a supported runtime or disable selection/download with a clear explanation before users spend bandwidth/storage. A supported advertised model must pass an actual plan and inference smoke test.

## R36 — Self-hosted WebSocket configuration drops authentication and accepts incompatible routing

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `plans_for` at line 193 and `Plan::SelfHostedWs` at line 124. A WS plan retains only the URL, discarding the saved API key; connection construction adds no bearer header. README advertises optional authentication for self-hosted protocols. Disabling streaming with a WS URL instead creates an HTTP-compatible plan whose base is `ws://`/`wss://`, which an HTTP request cannot use. An existing test asserts this routing rather than rejecting it.

- Root cause/correction: WS plan carries saved Bearer key. Upgrade request installs auth. Reject disabled streaming with ws/wss and invalid schemes.
- Regression coverage and executed validation: Corrected protocol-routing regression passes.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Loopback requiring Bearer and actual authenticated WSS, accepted HTTP/WS toggle matrix.

**Acceptance requirement:** Change/verify: carry auth into upgrade headers and validate URL/protocol combinations. Test a loopback WS server requiring Bearer authorization, plus streaming toggles for each accepted URL scheme.

## R37 — Claude cleanup thinking requests are invalid and response parsing misses text

**Grouped subissues and baseline evidence:**

**Inspected and checked against provider documentation.** [transcribe.rs](../src/transcribe.rs), Claude branch at line 3266 sets `max_tokens=1024` and `budget_tokens=10000`, contrary to the [manual thinking budget rules](https://platform.claude.com/docs/en/build-with-claude/extended-thinking). The configured default Haiku 5.5 also requires newer thinking configuration according to that page. Parsing only `/content/0/text` ignores final text when the first block is thinking. No paid API call was made.

- Root cause/correction: Model-aware Claude request helper uses adaptive/manual thinking appropriately; manual budget below output maximum. Extract text blocks by type.
- Regression coverage and executed validation: Request fixture checks for supported manual/adaptive/default/off modes pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Full model capability matrix including unsupported off modes, live disposable credentials for supported contracts.

**Acceptance requirement:** Change/verify: model-capability-aware thinking configuration and extraction of text blocks by type. Use request/response fixtures and a live disposable credential smoke test for each supported thinking mode.

## R38 — Cleanup accepts truncated or noncanonical replies as complete dictation

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), cleanup providers around lines 3180–3320. Claude's fixed 1024-token ceiling can truncate long speech; termination reasons are not checked. Other providers' completion/finish reasons are also ignored. Google concatenates every text part without excluding thought parts if returned. The cleanup prompt permits intentional empty output, but `post_process` treats empty output as provider failure and eventually pastes the original text.

- Root cause/correction: Reject truncated/abnormal Claude/Gemini/chat/Ollama completion; exclude thoughts. Completed empty answer succeeds and suppresses delivery instead of restoring raw speech.
- Regression coverage and executed validation: Thought/multi-block, completion reasons, malformed reply and empty-policy fixtures pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Long live transcripts, provider-specific model token controls and capability-aware Gemini thinking requests.

**Acceptance requirement:** Change/verify: distinguish complete, partial, empty-by-policy, and failed responses; extract canonical answer blocks only. Test long transcripts, token-limit replies, thought blocks, and filler-only input.

## R39 — Audio retention differs by engine and ignores a changed privacy preference

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), streaming return paths at lines 2828–2864 and audio write around line 3409. Streaming utterances return no WAV despite the whole recorder being available, so Keep audio silently excludes successful streaming/Whistle runs. File retention snapshots the switch at press; `deliver` can still write the provided WAV after the user has turned audio retention off, because it does not recheck that flag before writing.

- Root cause/correction: Streaming/Whistle capture can produce a WAV. Persistence begins an immediate database transaction, checks the original capture privacy/reset epoch and current privacy flags, and writes private audio under the same writer ownership as its history row. Old off/on captures cannot regain permission.
- Regression coverage and executed validation: Three capture_privacy_regressions pass, including off/on/reset/clear rejection, cancellation, private WAV/text persistence and associated deletion.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: F01 proves blocked-provider cancellation and empty history/audio after Cancel or durable policy loss. Successful streaming/file retention and complete clipboard/paste privacy boundaries remain.

**Acceptance requirement:** Change/verify: apply one documented retention policy across engines and recheck privacy immediately before persistence. Test streaming/file success and switching audio retention off while finalization is delayed.

## R40 — Retained audio and temporary files are not protected like the database

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), audio vault write around line 3418, keyword file around line 1463, Whistle staging, and `server_logs` at line 822. WAV/keyword/log creation relies on process umask rather than private creation. With a common 022 umask, files can be readable by other local users where parent directories permit it. Shared predictable `/tmp` and `/dev/shm` paths also allow collisions/symlink interference. SQLCipher protects `state.db`, not these artifacts.

- Root cause/correction: Private atomic vault file creation; secure unique tmpfs audio/keywords and private FIFO directory with RAII cleanup.
- Regression coverage and executed validation: Unique staging/0600 files and all tested Whistle exit cleanup pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Document/decide vault encryption, umask/conflict fixtures for every artifact, reset/delete coordination and full resource review.

**Acceptance requirement:** Change/verify: private directories/files from creation, unpredictable securely created staging paths, cleanup on all outcomes, and explicit vault encryption policy. Test permissions under umask 022 and preexisting conflicting paths using disposable locations.

## R41 — Retention maintenance only runs during successful delivery

**Grouped subissues and baseline evidence:**

**Inspected.** [transcribe.rs](../src/transcribe.rs), `deliver` at line 3388. Old history/audio are purged only when another transcript is delivered. Audio purge additionally requires Keep audio to remain enabled; turning it off leaves prior audio indefinitely unless manually cleared. History deletion/clear does not delete its corresponding WAV. Thus the UI's retention periods are not enforced while idle, and transcript deletion does not clear related audio.

- Root cause/correction: Retention runs on service startup/every minute and through an application-state-owned GUI worker independently of successful dictation or storage toggles. The GUI worker uses its own connection and wakes promptly on owner shutdown; previews do not start it. Pruning uses one indexed lookup per clip instead of copying all live history, under the database writer lock. Transcript deletion/clear and received tombstones also prune audio. Both retention controls remain editable with new storage off and preserve custom values.
- Regression coverage and executed validation: Injected forward/backward clock idle-retention regression redacts old text, removes expired/orphaned clips, preserves fresh clips and cleans up later. Direct transcript deletion removes its WAV. GUI-worker regression verifies startup/idle expiry with recording off and actual task exit on owner drop, including interrupting an hour-long timer. Zero/boundary/malformed-settings checks preserve history and the original malformed blob; background-open regression refuses removed databases and symlinks. Both native GTK configurations exercise custom/zero policy display, editable controls with storage off, and actual persistence on selection.
- Migration/compatibility: Transcript retention zero means forever; audio zero keeps the existing one-day lower bound and is displayed as one day. Turning storage off stops new writes; it does not suspend retention of existing data. Custom numeric periods are preserved. Invalid settings remain available for repair, with sync/new storage disabled. Final filesystem/migration review is pending.
- Unresolved requirements: Actual long-running GUI/service idle acceptance and filesystem failure/interrupted cleanup recovery remain. Startup/admission errors are logged; visible maintenance-failure feedback still needs review.

**Acceptance requirement:** Change/verify: periodic/startup retention maintenance and a clear per-transcript audio-deletion contract. Advance time with no new dictations and with recording disabled; expired files/text must be handled as promised.

## R42 — Sync restart and address/discovery handling are unreliable

**Grouped subissues and baseline evidence:**

**Inspected.** [service.rs](../src/service.rs), stop/start at lines 203–258; [net.rs](../src/net.rs), dialing at line 480, mDNS around line 680, and `watch_discovered` at line 811. Stopping sets a flag but does not join the accept loop before rebinding the same port; an immediate restart can fail, after which the watcher records the configuration as applied and waits for another edit. DNS/handshake errors on the first address short-circuit remaining candidates, and only the first resolved address is tried. IPv6 mDNS addresses lack brackets. The service appearance watcher does not persist newly discovered dial addresses; only the GUI's Nearby refresh does. Removed services remain in discovery, preventing reliable reappearance detection.

- Root cause/correction: SyncEngine serializes replacement before rebinding and owns/joins the listener plus periodic/discovery watchers. Watchers retry failed startup, including listener-disabled startup. Port zero preserves browsing/outbound catch-up without advertising an incoming listener. mDNS has an owned, cancellable browser and acknowledged daemon shutdown; GUI owners hand joining to the bounded reaper. Dial tries candidates/IPs with scoped IPv6 parsing and cancellable connect/handshake progress. Discovery limits names, addresses and cached services; the watcher persists addresses without holding the discovery mutex across database work, and removal keeps nodes still advertised through another service.
- Regression coverage and executed validation: Disposable native Secret Service probe passes three same-port replacements while clients stall, old-socket closure, watcher joins, immediate rebinding, and listener-disabled encrypted catch-up. Two actual multicast advertisers/browsers discover each other and process goodbye disappearance. Pending TCP/backlog and Noise-handshake cancellation regressions pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Actual DHCP/reappearance, dual-stack/scoped IPv6 network acceptance, desired-state recovery after discovery/worker failure, and multi-process service restart remain. Completed native multicast gate is same-host and uses ephemeral identities, not a cross-host LAN acceptance test.

**Acceptance requirement:** Change/verify: join listener shutdown, retry failed desired state with backoff, try all usable addresses, format IPv6 correctly, and update/expire discovery in the engine. Test rename/restart at a fixed port, stale first address, IPv6, DHCP changes without GUI, and disappear/reappear.

## R43 — Pairing and gossip need durable revocation and bounded reachability metadata

**Grouped subissues and baseline evidence:**

**Inspected.** [store.rs](../src/store.rs), `decide_request` around line 1026, `remove_peer` at line 950, and `apply_gossip` at line 1067; [net.rs](../src/net.rs), exchange contact bookkeeping around line 369. Accept/manual trust automatically grants introducer authority. Removing/declining deletes the row, so an introducer can add the device back as trusted. There is no durable reject/revoke marker or UI choice for introduction authority. Outgoing sync sends no listening addresses; the responder can record the dialer's ephemeral source port as a dial target. Addresses accumulate without expiry, and stale gossip overwrites custom names without versions.

- Root cause/correction: Durable revoked IDs prevent gossip repair; explicit trust clears revocation, accepting trust no longer grants introducer automatically. Listener addresses advertised; responder ignores ephemeral source port.
- Regression coverage and executed validation: Reset/revocation/gossip/explicit repair checks pass.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Introduction authority UI/policy, bounded expiring addresses and versioned names; real asymmetric pairing without mDNS.

**Acceptance requirement:** Change/verify: explicit introduction policy, durable local revocations, listener-only advertised addresses, bounded/expiring address metadata, and versioned names. Test unpair→gossip, asymmetric pairing without mDNS, and repeated connections.

## R44 — Full-state synchronization scales poorly and applies partial rounds

**Grouped subissues and baseline evidence:**

**Inspected/design risk.** [net.rs](../src/net.rs), payload export/merge around lines 415–450; [store.rs](../src/store.rs), export/apply methods. Every round exports all words/snippets/history/tombstones and applies records one at a time, even when unchanged. Multiple round triggers can run concurrently. Cross-table application has no transaction, so a failed round can partly commit. Permanently retained rows make the 256 MiB message cap an eventual hard failure. No negotiated schema/features accompany the single protocol version.

- Root cause/correction: Atomic batched typed-table merge; unchanged records do not bump generation. Negotiated pages carry one consistent upper sequence, newest-key change tracking and committed receive progress; each cross-table page merge/cursor is atomic. Database byte checks and bounded escaped-JSON counting precede allocation/encoding. Mutations use SQLite sequences rather than conflict clocks, so earlier keys and clock rollback cannot fall behind a resume cursor. Background triggers use a bounded per-database/per-peer scheduler: one active round and one latest dirty request per peer, independent of dictation workers. Startup/periodic/discovery/change requests retain their original permissions and engine owner. Enumeration continues in bounded batches instead of dropping peers on admission pressure.
- Regression coverage and executed validation: Rollback/idempotent generation/concurrent merge checks pass. Scheduler regressions verify dirty replay/fairness, unrelated-peer progress and panic recovery. A real two-round encrypted loopback changes data during the first exported snapshot and verifies the latest change arrives through one coalesced replay after 100 triggers. F07 adds a >16 MiB encrypted restart/resume, lost-ACK, fresh-peer recovery, exact Unicode preservation, steady-state zero-history and deletion/dictionary delta regression. Five store regressions cover transaction rollback including cursor, competing rounds, 65-row multi-page late mutations, clock anomalies, permission loss, bounded latest-key metadata, valid multi-megabyte individual Unicode text and escaped-wire rejection without data loss. The aggregate-page checkpoint passed 212 tests. The schema-8 segmentation correction adds ten store regression groups and actual signed Noise slow-link/lost-ACK acceptance: one >16 MiB individual Unicode record survives reopen, repeated interrupted rounds and final atomic publication; malformed complete records/corrupt final bytes cannot merge or advance progress. Clear/reset preserve pending deletion identities; revocation/untrust/settings changes remove staged bytes; source deletion retires old snapshots; retention, idle expiry and backward clock handling are exercised. Full Rust suite passes 242 tests, zero failed, with the original five opt-in ignores unchanged. Actual schema-7/current protocol and killed-migration recovery checks pass.
- Migration/compatibility: Schema 7 adds the durable change ledger and per-peer receive cursors; schema 8 atomically adds encrypted durable record/chunk staging with policy, source-mutation and peer-lifecycle cleanup triggers. Segmentation is negotiated independently of paged transfers: old schema-7 peers retain paged interoperability for supported records and receive an explicit upgrade requirement for oversized records. A SIGKILL after migration SQL/before COMMIT leaves schema 7 readable/writable by the actual old binary; successful upgrade/reopen preserves history, settings, words and the ledger; that actual old binary refuses schema 8 without changing the file. Segmented records allow at most 64 MiB decoded text, 384 MiB plus metadata of worst-case JSON, 512 MiB aggregate encrypted staging and 16 transfer states. Incomplete stages expire after one hour of idle time or restart conservatively after clock rollback. Source data is never dropped to enforce these budgets. Existing content, settings, keys, replicated tombstones and revocations remain intact; no identity/key rotation. Reset and peer removal clear receive progress. Peers without paging retain small whole-snapshot compatibility and require an explicit upgrade for larger snapshots, before whole-state vector allocation. Tombstones remain until a safe distributed retirement policy can be established. The previous schema-6/page compatibility evidence remains preserved separately.
- Unresolved requirements: Manual/inbound/cross-process scheduling fairness, tombstone retirement policy, very large aggregate-history scale, behavior above the explicit 64 MiB individual-record limit, and final allocation/filesystem/privacy/compatibility review. Actual slow-link and interrupted individual-record acceptance now passes; it does not establish every deployment or final packaged artifact. Concurrent page imports fail safely on stale cursors rather than skipping data; further process-wide coalescing remains under review.

**Acceptance requirement:** Change/verify: coalesce per-peer rounds, batch transactional merges, delta/change tracking and a tombstone lifecycle; explicitly version compatibility. Benchmark a large history over a slow link and inject failure halfway through a merge.

## R45 — Clipboard copy reports success before the command succeeds; restoration is incomplete

**Grouped subissues and baseline evidence:**

**Inspected.** [clipboard.rs](../src/clipboard.rs), copy at line 176, reaping at line 202, and pipeline at line 287. After writing stdin, copy returns success without checking the child status or selection readiness. A failed `wl-copy` can therefore be followed by pasting the old clipboard. If the previous clipboard was empty/unavailable, Keep in clipboard off has nothing to restore, so the transcript remains. Fixed 120/250 ms sleeps are not proof of selection consumption; a delayed target or a user clipboard change can be overwritten.

- Root cause/correction: One bounded, process-owned Wayland worker retains explicit source generations, waits for compositor acknowledgement and actual completed pipe transfers, snapshots empty/multiple MIME offers, and restores only after processing source cancellation. Copy/paste failures propagate to the owning finalization. Privacy/cancellation guards interrupt queued/active work; bounded foreground wl-copy fallback preserves copy/Keep-on compatibility where data-control is absent.
- Regression coverage and executed validation: Four selection regressions pass: large partial Unicode transfers/EOF, old-source/preload/marker exclusion, bounded stalled/closed/excessive recipients, and cancellation of an unresponsive compositor. Disposable real niri/wtype/GTK target passes 700 ms delayed consumption, empty/text/binary restoration, user replacement, cancellation cleanup, copy-only and no-op behavior (`scripts/clipboard-smoke.sh`).
- Migration/compatibility: Persisted WlCopy enum remains readable; implementation uses ext-data-control with wlr compatibility. Copy/Keep-on fallback retains a foreground owned wl-copy. Safe borrowing requires source-cancellation support and reports that requirement on unsupported compositors. Clipboard snapshot/transfer limits are explicit failures; no silent format truncation.
- Unresolved requirements: Fallback and compositor protocol variants still need live validation; evaluate clipboard-manager reads and the protocol limitation that set_selection has no atomic ownership-conditional restoration. Compositor connection uses a nonblocking bounded socket; GTK copy callbacks enqueue and poll through weak widget owners. Final live rerun of these latest changes remains required.

**Acceptance requirement:** Change/verify: wait for successful selection ownership with a deadline, represent empty clipboard explicitly, and restore only while still owning the selection. Use fake CLI fixtures for failure/readiness plus a slow Wayland target and user copy during delivery.

## R46 — GTK signal closures retain dictionary widgets after removal

**Grouped subissues and baseline evidence:**

**Reproduced.** [app_dictionary.rs](../src/app_dictionary.rs), `word_card` at line 96 and analogous snippet callbacks. A card owns its buttons, while button signal closures strongly capture the card. Removing the card/page does not break the cycle. The probe dropped the dictionary page and verified the card's weak reference still upgraded. Periodic rebuilds after data-generation changes and language rebuilds can accumulate retained cards and app-state references. Similar strong widget captures merit review in forms/chain lists.

- Root cause/correction: Dictionary/snippet cards, STT/LM forms, list rows and settings navigation capture weak widgets. Form upgrade is confined to one callback. Shortcut capture references controls weakly. Removed sync status widgets stop their timers; reachability probes are admitted as owned bounded tasks without overlapping probes.
- Regression coverage and executed validation: Native regressions pass for 20 dictionary cards and three complete settings-page rebuilds, asserting removed fields/buttons/expanders are released. The settings check demonstrably failed before the correction. Both light/en and dark/es native smokes pass, including immediate sync control/status-widget release and capture page suspension lifecycle. Strict Clippy passes.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: The GTK 4.24.1 selectable-label focus path borrowed the previous widget across a reentrant parent focus transition. A synthetic notification handler that removes the previous focused button reproduces the same fatal gtk_widget_is_ancestor critical before correction. Window focus ownership now retains retired widgets through dispatch, drops them outside state borrows, and releases all owners on window destruction. Both native configurations pass sixteen repetitions asserting the old widget survives dispatch, is released afterward, the closed window is released, and selectable labels remain supported. The original dictionary cancellation and all 36 navigation/settings assertions pass unchanged. See validation-review-focus.json. The X11 overlay capability probe now checks the display backend before calling the Wayland-only layer-shell probe. Complete populated row coverage and longer repeated edit/navigation/language memory validation; final native reruns after all remaining implementation changes.

**Acceptance requirement:** Change/verify: weak captures or explicit signal disposal; avoid rebuilding unchanged records. Repeated refresh/edit/navigation/language cycles must release removed widgets and keep memory stable.

## R47 — Opening Settings changes the active remote engine's language

**Grouped subissues and baseline evidence:**

**Reproduced.** [app_settings.rs](../src/app_settings.rs), `stt_lang` at line 1963 and refresh around line 2823. The global transcription-language list follows the local model even if the local option is absent/disabled and the active chain is entirely remote. Rendering the dropdown corrects unsupported values and persists that correction. A Spanish remote-only configuration becomes English merely by opening Settings when the remembered local model is English-only.

- Root cause/correction: Remote active chains offer global languages and preserve unknown stored codes. Rendering never changes the operational choice, including an unsupported local-only value. Local plan creation rejects incompatible language/model combinations with an actionable correction; an enabled remote fallback retains its configured language.
- Regression coverage and executed validation: Both native UI configurations assert Spanish remains selected and unchanged with a remote-only chain remembering an English-only Nemotron model and after removing the remote. Unsupported stored choices remain editable. A plan regression verifies local rejection, valid auto/model languages and unchanged Spanish remote fallback. All pass.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Final diff/validation reruns; real provider language contracts are tracked separately under provider integration gates.

**Acceptance requirement:** Change/verify: provider-specific language configuration or a remote-capable global list; rendering must not silently mutate operational settings. Test remote-only/non-English chains with a different local model remembered.

## R48 — Release packaging and prerequisite handling are incomplete

**Grouped subissues and baseline evidence:**

**Inspected; version failure reproduced.** [packaging/nixos/flake.nix](../packaging/nixos/flake.nix), lines 17–34, has a placeholder hash. After correcting it, `fetchzip`'s default root stripping conflicts with the extra inner-directory search in `installPhase` ([Nixpkgs fetcher documentation](https://github.com/NixOS/nixpkgs/blob/master/doc/build-helpers/fetchers.chapter.md)). Arch/Nix dependencies omit default delivery tools `wl-clipboard`/`wtype` and capture/discovery tools `parec`/`pactl`; the README's system dependency list is incomplete. Release binaries require GTK 4.22 through Cargo features, while package constraints do not express a supported ABI baseline. [packaging/arch/update.sh](../packaging/arch/update.sh), line 27, uses `enable --now` without restarting an already active engine, leaving old code running after an upgrade. [bin/mywhisper.rs](../src/bin/mywhisper.rs), line 18, initializes DB/keyring/identity before processing `--version`; the release binary panics without Secret Service instead of printing its version.

- Root cause/correction: Informational CLI flags precede DB/keyring initialization. Arch expresses GTK 4.22/libadwaita 1.5 and default delivery/PulseAudio tools. Upgrades build in a disposable directory and restart the service. Nix builds authenticated source against locked Nixpkgs, uses the correct `cargoBuildFlags` to package only `mywhisper`, and includes all resolved third-party notices. The NixOS module supplies nix-ld/CPU libraries for downloaded engines, with explicit remote-only opt-out and separate CUDA prerequisites.
- Regression coverage and executed validation: Disposable-store Nix source builds pass with tests enabled; notice collection checks all 370 dependencies, package contents exclude the development pill executable, and isolated packaged `--version` passes. NixOS module evaluation checks loader/libraries/service configuration. A verified local candidate passes clean rootless Arch makepkg/install/pkgrel upgrade, ELF dependencies, no-keyring/no-data CLI, desktop/service definitions, notice/icon installation and package file integrity. Five upgrade and eight release/notice regressions pass. These are recorded snapshots and will be repeated on final code.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Clean NixOS desktop/downloaded-runtime loader compatibility; running Arch desktop/service upgrade and recovery; final packaged artifact/live integration checks. A userspace package upgrade does not establish running service restart or desktop integration.

**Acceptance requirement:** Change/verify: make advertised packages build/install unmodified, supply or clearly require runtime tools, define/test the ABI baseline, restart the upgraded service, and parse informational CLI flags before initialization. Test clean installation and upgrade in disposable Arch/NixOS desktop environments with and without prerequisites.

## R49 — Download cancellation, integrity, and installation are incomplete

**Grouped subissues and baseline evidence:**

**Inspected.** [stt.rs](../src/stt.rs), `download_model` at line 448 and runtime downloaders at lines 898, 1166, and 1329. HTTP clients disable the overall timeout and do not set read/connect deadlines; cancel is checked only between blocking reads, so an accepted but stalled response cannot be canceled promptly. Models use mutable upstream URLs and verify only a lower size bound or file presence, while runtime binaries do have SHA-256 verification. Model/archive extraction writes into the final installed directory; failure can leave partial required files that presence-only installation checks accept. Delete/reset/cache-clear can race active download/extraction because there is no shared install lock or coordinated cancellation. A failed/retried partial extraction is not an atomic replacement.

- Root cause/correction: Pinned model URLs/digests/exact sizes, interruptible async request/body progress, hard transfer/archive bounds, private staging, safe archive links and atomic directory exchange. Shared per-asset/global locks and epochs invalidate active delete/reset work. Registry admission is atomic, bounded and identity-checked; long downloads use owned workers. GTK deletion/cache clearing runs off-thread. F09 adds authenticated content receipts for complete archive trees and exact pinned raw-file checks, with bounded off-thread UI verification. Private control reads reject non-regular files without waiting; verification rechecks cancellation and the original control identities/epochs between chunks. Full staged verification precedes promotion so a failed update preserves the previous installation.
- Regression coverage and executed validation: Six artifact regressions pass: stalled headers/body cancellation, corrupt/truncated/oversized/writing-failure responses, safe/escaping archive links, extraction limits, atomic replacement, and active delete/reset. Actual CPU pack and Tiny model downloads pass (including SHA-256, exact model bytes, extraction and private layout).
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Queued cross-process admission/reset barriers, interrupted promotion/disk-full/extraction cancellation, dedicated native legacy-verification/local-trust acceptance, actual remaining assets and bounded-worker admission regression remain. Provider admission, active-use asset ownership and non-destructive legacy/manual compatibility have targeted corrections and seven additional regression groups in validation-review-asset-ownership.json; final integration/resource review remains. Download DNS also needs an owned cancellation audit; a request timeout alone does not establish resolver cleanup. Receipt/source/key tampering and control replacement now have passing primitive coverage under F09, but the complete repair and provider path remains open.

**Acceptance requirement:** Change/verify: bounded/cancellable network IO, pinned model hashes or explicit integrity validation, private staging plus atomic promotion, and shared ownership for download/delete/reset. Test a response that stalls after headers, canceled extraction, disk-full, malformed model, retry after partial install, and deletion during a download. Validate the actual pinned upstream assets in release integration checks; this audit did not download all multi-gigabyte models.

## R50 — Settings/state boundaries and documented architecture have drifted

**Grouped subissues and baseline evidence:**

**Inspected.** [app.rs](../src/app.rs), state load at line 55; [app_settings.rs](../src/app_settings.rs), preference persistence; [store.rs](../src/store.rs), `save_settings` at line 332. The GUI holds and writes a whole settings snapshot rather than atomically updating individual preferences, so another frontend's changes can be overwritten. Dictionary content is duplicated in that JSON despite typed tables being authoritative, leaving stale sensitive snippet expansions in the blob until another save. Store opening runs schema/migration/update work repeatedly from watchers; even unchanged sync merges bump generation and rebuild widgets. `checkpoint` has no caller despite comments promising clean-shutdown external-file replication. Comments describe GUI engine ownership, a future P2P layer, user-placed Needle, no audio retention, and pool-only background work that no longer match runtime behavior. Many direct thread spawns remain outside the bounded runtime, and GTK callbacks synchronously perform microphone discovery, media D-Bus operations, process probes, and database work. These compound the lifecycle/performance problems above.

- Root cause/correction: Transactional settings snapshot-diff preserves another frontend changes; settings saves acquire SQLite write ownership before reading the stored blob, avoiding deferred-read snapshot upgrade failures during concurrent contact/settings writes; typed dictionary remains authoritative and is omitted from JSON. No-op merges do not rebuild. Schema work only on version upgrades. Runtime init serialized. Server status TCP/process inspection now runs on one page-owned bounded session task, with a nonblocking GTK result poll; hidden, removed or reconfigured pages cancel pending work. Results belong to the originating page and configuration, and callbacks hold weak status controls.
- Regression coverage and executed validation: Two-frontends/dictionary privacy/idempotent generation and concurrent DB tests pass. Real loopback cancellation coverage verifies that a cancelled status query makes no connection and cancellation between engines prevents the next connection. Native smoke now exercises real mapped status queries, a changed configuration with an owned reserved non-listening port, foreign-process stop refusal, and removed speech/settings control release. The original checkpoint passed 165 tests. Final-check concurrency exposed SQLITE_BUSY/SQLITE_BUSY_SNAPSHOT in settings saves. A new two-connection, 64-round barrier regression reproduces failures before the correction and passes after immediate transaction admission; both disjoint permission edits and an unknown JSON field survive. The expanded all-target Rust suite passes 243 tests with zero failures and the five unchanged opt-in runtime tests; strict Clippy and formatting pass. Broader GTK/database ownership remains open.
- Migration/compatibility: Schema v6 retains replicated tombstones and durable local revocations; legacy peer compatibility and interrupted upgrade review remain open.
- Unresolved requirements: Explicit service/session/provider boundaries, backup/export policy, remaining GTK blocking operations, task ownership and obsolete docs/helpers.

**Acceptance requirement:** Change/verify: establish clear repository/service/session/provider boundaries, transactional preference patches, one authoritative dictionary store, read-only polling, explicit task ownership and cleanup, and accurate support docs. Test two frontend snapshots, startup/rebuild latency with slow external services, and any promised backup/export workflow. Remove obsolete helpers/comments rather than treating them as functioning features.

## R51 — Release automation has no stable-version gates or artifact integrity checks

**Grouped subissues and baseline evidence:**

**Inspected/check failures reproduced.** [.github/workflows/release.yml](../.github/workflows/release.yml) builds and publishes arbitrary `v*` tags without tests, fmt, Clippy, dependency audit, `--locked`, or tag/package version comparison. The current package/Arch/Nix versions are 0.9.2; tagging 1.0.0 alone can publish a tarball whose binary still identifies as 0.9.2. Arch uses `sha256sums=('SKIP')`. Release creation does not mark prerelease tags as prereleases, and update paths select the first release without an explicit stable-only policy. The fallback changelog compares the tag with itself. Existing fmt/Clippy failures are mainly hygiene, not 88 independent functional bugs. No project LICENSE/third-party notice artifact ships despite source files describing MIT-ported code; resolve the intended license and preserve applicable notices before distribution.

- Root cause/correction: Stable update offers reject draft/prerelease/malformed tags and compare numeric versions, with bounded subprocess/output and check generation ownership. Arch baseline checksum is pinned to the authenticated published asset; updater checks authoritative SHA-256 before package extraction. The release workflow now checks exact tag/Cargo/Arch/Nix/binary consistency, fmt/strict Clippy/all-target tests, current security warnings, native UI/keyring and packaging regressions before staging; publishing is a separate minimal-permission job that checks the original source/tag and checksum again. Prereleases are marked explicitly and excluded from stable updates. Notes require a matching nonempty changelog section. Candidate assets include checksums, source/dirty/licensing metadata and collected third-party notices, including the exact upstream OpenWhispr MIT notice. No project LICENSE added, per user instruction; distribution is explicitly held.
- Regression coverage and executed validation: Five upgrade and eight release/notice regressions pass. Verified actionlint 1.7.12 passes the workflow. A disposable candidate archive passes layout/SHA256, clean Arch installation/pkgrel upgrade, its binary version and ELF dependencies. Nix source packages also preserve notices. The candidate explicitly records dirty local source and is not distributable/final. Strict lint/build/security/native UI checks pass as interim checks, and must be repeated on the final diff.
- Migration/compatibility: Supported behavior must be preserved. The final compatibility review is pending.
- Unresolved requirements: Candidate notice generation covers all 370 resolved locked crate versions. The 20 crates whose archives omit workspace licenses are tied to exact crate checksums/source commits and frozen upstream blobs; provenance and corrupted-text regressions pass. Final candidate/desktop checks and CI execution remain. Project licensing is deferred by the user; distribution remains blocked. No tags/releases/PRs have been created.

**Acceptance requirement:** Change/verify: require release checks and version consistency, publish/check hashes, define stable/prerelease selection, generate meaningful release notes, and include licensing metadata/notices. Dry-run a candidate release and verify the unpacked artifact and package manifests.

## External gates and user decisions

- Nix 2.35.2 works with disposable store `local?root=/tmp/mywhisper-nix-audit-store`; source builds with all tests pass, packaged CLI/notice checks and NixOS module evaluation pass. Original native hotkey assertions remain in a required live companion gate, which passes locally with exclusive ownership of its synthetic devices. The system daemon remains untouched. This is not a clean NixOS desktop validation.
- Rootless Podman provides clean Arch userspace. Candidate makepkg/install/pkgrel upgrade passes with no host credentials/data/devices mounted; a running desktop/user-service upgrade and NixOS desktop remain outstanding.
- Local GPU is NVIDIA RTX 3090 (24 GiB), driver 615.71.09. QEMU and FFmpeg are available. Host compositor is Wayland/niri. Virtual uinput keyboard tests run locally. Actual advertised microphone formats/device changes, compositor paste/shortcuts and paired discovery/revocation still require acceptance evidence.
- No disposable cloud credentials have been identified. No paid cloud calls have been made. Real user settings, transcripts and keyring secrets have not been copied into tests.
- User decision: leave project LICENSE for later. Do not invent a license/copyright holder. Preserve third-party notices; release distribution is held pending an explicit licensing decision.
- Required final fmt/Clippy/all-target tests/release build/current security audit/both UI smokes/artifact/install/upgrade/live integration gates remain unsubmitted until all pass.

## Evidence integrity

The published evidence directory tracks repro.rs, ui_probe.rs, reproduce.sh and dependencies.json. Separate normalized console logs mentioned in the report are not present in the immutable commit. Historical probes deliberately assert failures and are not the corrected regression suite. New corrected regressions reside beside the implementation and use disposable inputs.
