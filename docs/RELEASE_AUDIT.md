# MyWhisper v1.0.0 release audit

**Verdict: hold the stable release.** The optimized build, existing tests, and UI smoke checks pass, but they miss failures affecting privacy controls, replicated deletions, recording ownership, cancellation, and settings. This audit identifies **51 findings: 17 P1, 33 P2, and 1 P3**. Some findings group related defects in the same subsystem.

Audited on 2026-10-09 at commit `d2228a24654dae213a69d06d4ca3ec6544dde22c`; package version is currently `0.9.2`. Production code was not modified. The only additions are this report and its evidence/reproduction files.

## Scope and evidence

Reviewed all 52 tracked files: Rust modules and binaries, settings and migrations, GTK pages and callbacks, recording/resampling, transcription and cleanup providers, model/runtime downloads, encrypted sync and discovery, identity and storage, service/process ownership, clipboard/media/sounds, translations, examples/tests, release automation, Arch/NixOS/systemd packaging, documentation, CSS, and asset definitions. The review followed cross-module behavior as well as individual functions; it is not a claim that every possible execution path was exercised.

| Check | Result |
| --- | --- |
| `cargo test --locked --all-targets` | 77 passed, 5 ignored; no failures |
| `cargo build --release --locked --bin mywhisper` | Passed |
| Native UI smoke, Xvfb, English/light | Passed all 37 steps |
| Native UI smoke, Xvfb, Spanish/dark | Passed all 37 steps |
| `cargo fmt --all -- --check` | Failed in `service.rs` and `transcribe.rs` |
| `cargo clippy --locked --all-targets -- -D warnings` | Failed: 77 diagnostics for the library, 88 including library tests |
| cargo-audit 0.22.2, 469 locked dependencies | No known vulnerabilities or informational warnings; RustSec database commit `7eebec69c352c7191b1f13eb95dd510eeca5d1de`, updated 2026-10-09 |
| Focused executable probes | 13 reproduced core failure scenarios, 3 GTK failure scenarios, plus release binary `--version` failure without a keyring |

**Reproduced** means an executable probe demonstrated the described failure. **Inspected** means a concrete source path establishes the defect or race, but the complete hardware/network scenario was not exercised. **Design risk** means the implementation needs an explicit release decision or additional fault testing; it is not a claim of observed corruption or exploitation.

Evidence is in [release-audit](release-audit/). Stored console logs have trailing whitespace normalized. [repro.rs](release-audit/repro.rs) uses disposable databases, ephemeral identities, and loopback encrypted sync; it never accesses real transcripts or sends them to another device. [ui_probe.rs](release-audit/ui_probe.rs) uses an in-memory GTK app state. Run the focused probes with `bash docs/release-audit/reproduce.sh` (requires Rust, Python, ripgrep, and Xvfb). The invalid-discovery probe injects the same data shape accepted from mDNS; it does not spoof an actual LAN advertisement. The queued-press and replacement-recorder probes isolate the scheduling/ownership failures without using a microphone. The speculative-cancellation probe uses a placeholder model file and no inference runtime; the leaked loops are demonstrated without decoding audio.

No live paid cloud requests, actual model inference, CUDA/Vulkan execution, Bluetooth capture, real keyboard/compositor injection, real Secret Service provisioning, actual Nix builds, or installation/upgrades on another machine were tested. Nix is unavailable here. The five ignored tests cover downloaded runtimes/models and inference. These limits matter even though the build and existing tests pass.

## P1 — resolve before stable release

### R01 — Sync off still allows transcript transfer

**Reproduced.** [net.rs](../src/net.rs), `sync_all_trusted` at line 732, loads settings but does not check `sync.enabled`. `sync_after_change` and delayed startup sync invoke it independently of the listener's stop flag. Dictionary edits and history deletion call this path from the GUI. Turning off the sync engine therefore does not stop outgoing synchronization, including sending complete transcript history. The probe saved `sync.enabled=false`, performed encrypted loopback sync, and verified the transcript on the receiving database.

**Change:** gate every outgoing path and every new exchange on the authoritative master setting; propagate cancellation to in-flight work. **Verify:** disable sync before and during a round, edit dictionary/history, and assert no records leave the device.

### R02 — Outgoing sync does not enforce local peer trust

**Reproduced.** [net.rs](../src/net.rs), `exchange` at line 300 and `sync_now` at line 471. The local `trusted` value is enforced only by the responder. An initiator authenticates the expected identity and accepts the remote verdict, then exports its records even when its own peer row is untrusted. This also leaves a revocation race after a worker snapshots its trusted-peer list. The probe made B trust A while A explicitly distrusted B; A still sent history to B.

**Change:** require local trust immediately before exporting records in both roles, with revocation semantics for active exchanges. **Verify:** asymmetric trust and revocation between dial and exchange must reject all content transfer.

### R03 — Deleted transcripts retain their original content and are exported

**Reproduced.** [store.rs](../src/store.rs), `delete_history` at line 443, `clear_history` at line 462, `purge_old_history` at line 614, and `export_history` at line 659. Deletion only sets a flag; both final and raw text remain in the database and in sync payloads, including payloads sent to newly paired devices. Retention is consequently logical hiding, not removal of transcript content.

**Change:** keep identity-only tombstones and redact payload columns locally and on receipt; account for SQLite WAL/backups when defining deletion guarantees. **Verify:** delete/clear/expire a transcript, export to a new peer, and inspect both databases for the original text.

### R04 — A stale discarded record resurrects deleted history

**Reproduced.** [store.rs](../src/store.rs), `apply_history` at line 759. The conflict update assigns both incoming flags whenever either incoming flag is true. Thus `(deleted=true, discarded=true)` merged with older `(deleted=false, discarded=true)` becomes visible again. Arrival order changes the final result, violating convergence.

**Change:** merge monotonic flags with logical OR, or use an explicit versioned state machine; combine this with R03's redaction. **Verify:** all permutations and repetitions of normal/discarded/deleted snapshots converge and no deleted row returns.

### R05 — Reset retains trusted peers, enables sync, and permits deleted data to return

**Reproduced.** [store.rs](../src/store.rs), `reset_all_data` at line 603; [app_settings.rs](../src/app_settings.rs), reset action around line 4617. Reset physically removes content and settings but leaves `devices` and sync metadata. Default settings turn sync back on. Existing peers can then reintroduce the records whose tombstones were removed. The probe verified preserved trust, enabled sync, and resurrection from a previous history export.

**Change:** define reset as a coordinated boundary: stop jobs/sync, clear trust or retain replicated deletion state, then reset settings. **Verify:** reset a paired device and reconnect it to a stale peer; user data must stay cleared.

### R06 — First-run identity creation is not atomic

**Reproduced.** [device.rs](../src/device.rs), `load_or_create` at line 33. `exists` followed by `fs::write` lets the GUI and service generate different identities against the same file; concurrent readers may see partial contents. One probe run returned four distinct identities for one path. Legacy-key expansion has the same race. Permissions are tightened only after writing, and existing 64-byte keys are not permission-checked.

**Change:** serialize initialization/migration with a lock and atomically install a private file; reread the winning identity. **Verify:** concurrent first launches all return the persisted identity, including legacy migration and restrictive permissions from creation onward.

### R07 — First-run database-key creation can lose the key to an encrypted database

**Inspected race.** [store.rs](../src/store.rs), `db_key` at line 132; [bin/mywhisper.rs](../src/bin/mywhisper.rs), initialization at line 18. Two processes can both observe `NoEntry`, generate different keys, and overwrite the same keyring entry. Database opening occurs before acquiring the service engine lock. The final keyring secret may differ from the secret used to encrypt the new database. This was not reproduced against a live Secret Service.

**Change:** serialize key provisioning and database initialization across processes; preserve recoverability instead of treating all startup errors as fatal panics. **Verify:** simultaneous first GUI/service launches against a disposable Secret Service, followed by fresh-process reopening.

### R08 — Release/cancel can arrive before the queued press installs its session

**Reproduced.** [bin/mywhisper.rs](../src/bin/mywhisper.rs), Show handler around line 248; [transcribe.rs](../src/transcribe.rs), `begin_utterance` at line 2301 and `end_utterance_inner` at line 2693. Press is queued in the shared pool while release runs immediately. If press is delayed, release finds no session; the queued job then starts a recording that has already been released. Streaming connection setup also happens before publishing the live session, creating another lost-release interval. The probe occupied the four workers, queued press, released, and observed `has_live()==true` afterward.

**Change:** represent press/release/cancel as one generation-owned state machine before scheduling setup. **Verify:** fast taps, saturated workers, slow WS handshake, and cancel during setup never leave a live tap or mic.

### R09 — An old release disarms the next recorder

**Reproduced.** [transcribe.rs](../src/transcribe.rs), release-settle thread around line 2729; [audio.rs](../src/audio.rs), `set_recorder` at line 102. After waiting for the old recorder, the tail thread unconditionally clears the process-global recorder slot. A new press during that interval installs a new recorder which the old release then detaches. The replacement-recorder probe confirmed samples stop reaching it.

**Change:** compare ownership/generation before clearing the global tap. **Verify:** repeated press/release/press at intervals shorter than the flush delay, including configured release delay, preserves the new recording.

### R10 — Canceling speculative recordings permanently occupies workers

**Reproduced.** [transcribe.rs](../src/transcribe.rs), speculative setup around line 2360, `run_spec` at line 2149, and `cancel_utterance` at line 2586. Speculation has its own stop flag; cancellation, stale-session replacement, and overlapping-session drop never set it. Dropping the result receiver does not stop the loop. Four canceled speculative recordings consumed all four workers; a subsequent trivial job never ran.

**Change:** make session destruction cancel and join all owned tasks, and move long-running loops out of the short-job pool. **Verify:** hundreds of cancellations and replacements leave a fixed worker count and keep a sentinel task responsive.

### R11 — Unicode dictionary prompts panic; a panic can leave dictation permanently busy

**Reproduced helper panic; inspected busy-state consequence.** [textnorm.rs](../src/textnorm.rs), `dictionary_prompt` at line 28, slices UTF-8 at an arbitrary byte budget. A long CJK word with the 890-byte budget panics. This helper is reachable while finalizing a file transcription. [transcribe.rs](../src/transcribe.rs), worker around line 2750, resets `BUSY` and invokes completion only after finalization returns; the outer runtime catches panics without running those steps.

**Change:** truncate on character boundaries, enforce the actual provider budget, and use a finalization guard that always clears busy/completes. **Verify:** CJK/emoji/accented inputs crossing each budget, plus injected worker panics, allow the next utterance.

### R12 — An unauthenticated discovered device ID can panic the GTK UI

**Reproduced UI panic with injected discovery data.** [net.rs](../src/net.rs), mDNS resolution around line 680, accepts any nonempty TXT node ID. [app_sync.rs](../src/app_sync.rs), `short_id` at line 50, slices it at byte 8 and at `len-4`. A sufficiently long non-ASCII ID panics; arrival through the refresh timer would run inside a GTK callback, where unwinding can abort the process. Gossip can introduce malformed IDs too.

**Change:** validate canonical 64-character hexadecimal IDs at all boundaries and make display shortening UTF-8 safe. **Verify:** malformed IDs delivered through discovery and gossip cannot enter storage or crash a rendered device list.

### R13 — WebSocket setup and TLS streaming can block indefinitely

**Inspected.** [transcribe.rs](../src/transcribe.rs), `ws_connect_url` at line 1237, `ws_read_timeout` at line 1316, and finalization at line 2828. Socket timeouts are installed after `tungstenite::connect`, so TCP/TLS/HTTP upgrade has no application deadline. Both timeout helpers cover only plain streams; WSS has neither the short partial-read timeout nor the final-read timeout. A quiet/stalled server can pin a pump, then finalization waits on unbounded `recv()`, leaving Processing and the busy flag active.

**Change:** bound connect/upgrade/read/write/finalization for plain and TLS transports, with cancellation. **Verify:** servers that accept but never upgrade, never emit partials/finals, or stop consuming data recover within documented deadlines.

### R14 — Multichannel CPAL input is treated as mono

**Inspected.** [mic.rs](../src/mic.rs), `CpalMic::build_on` around line 267, uses the device's default channel count but feeds every interleaved sample to a mono recorder at the original frame rate. Stereo therefore becomes twice as many mono samples with altered timing and signal; all sample-format branches have this problem. It affects the fallback capture path used when `parec` is unavailable.

**Change:** negotiate mono or downmix complete frames before metering/capture. **Verify:** known mono/stereo/multichannel fixtures preserve duration, pitch, channel alignment, and recognition input format.

### R15 — Server port and exposure edits do not invalidate running-server identity

**Inspected.** [transcribe.rs](../src/transcribe.rs), server keys at lines 919, 1117, and 1186; reuse at line 736. Cache keys omit port and bind host. A live matching child returns success without moving it. Changing port makes clients dial a different endpoint; turning LAN exposure off can leave the original externally bound whisper server running.

**Change:** include all launch-relevant parameters in the supervised configuration and restart/rebind explicitly. **Verify:** edit port and exposure while warm, verify actual listening sockets, and successfully transcribe afterward.

### R16 — Default sync listener has no connection/concurrency budget

**Inspected resource-exhaustion paths.** [net.rs](../src/net.rs), `serve` at line 525, creates a new OS thread for every accepted connection. A Noise session does not require a previously trusted identity, and `recv_msg` at line 257 permits messages up to 256 MiB before the signed hello/trust check. Empty decrypted chunks can make no progress indefinitely; socket reads have per-call timeouts but no whole-message deadline, and writes have no timeout. Sync is enabled by default and binds all interfaces.

**Change:** cap active/pending connections, use small pre-authentication message limits, reject empty chunks, and enforce byte/progress/total deadlines plus write deadlines. **Verify:** bounded local load and slow-peer tests preserve dictation and a fixed memory/thread budget. No denial-of-service attack was performed during this audit.

### R17 — Raw and final transcripts are written outside the encrypted history store

**Inspected.** [transcribe.rs](../src/transcribe.rs), partial/segment logs around line 1402, speculative logs around line 2199, and `deliver` at lines 3353 and 3387. `eprintln!` records speech before checking history/privacy settings. In the systemd service it goes to the journal. The five-file tracing rotation in [paths.rs](../src/paths.rs) does not control these stderr logs. Disabling or clearing history consequently leaves another retained transcript copy.

**Change:** redact speech by default and make content logging an explicit, bounded diagnostic mode. **Verify:** with history disabled, capture the service journal/stderr and runtime logs while dictating synthetic secrets; none should contain speech.

## P2 — functional, architectural, packaging, and privacy issues

### R18 — Process adoption/reaping can target unrelated applications

**Inspected.** [transcribe.rs](../src/transcribe.rs), `reap_stale_servers` at line 473, `find_server_pids` around line 523, and `ensure_server` at line 696. Ownership is inferred from substring matches anywhere in `/proc/<pid>/cmdline`; adoption is not tied to the process actually listening on the selected port. A hand-started server or another program with a matching argument may be adopted or terminated. PID-file checks also accept the broad `sherpa-onnx` substring. The supposedly unconditional untracked sweep returns early when the PID map is empty.

**Change/verify:** supervise only verifiable children, record executable/start-time/configuration, associate adopted sockets with their process, and test alongside an unrelated same-user whisper server and commands containing the marker.

### R19 — LWW comparisons are not atomic with writes

**Inspected race.** [store.rs](../src/store.rs), `apply_words` at line 687 and `apply_snippets` at line 718, read a version and later unconditionally upsert. Multiple connections can both pass the comparison, then the older writer overwrites the newer one. Sync can run concurrently from several independent threads.

**Change/verify:** put the version predicate in the SQL conflict update or a transaction with correct contention handling. Exercise simultaneous local edit and two peer merges with barriers; the maximum version must always survive.

### R20 — Wall-clock LWW loses fast local edits and traps future-dated data

**Reproduced.** [store.rs](../src/store.rs), `now_ms` at line 23 and local CRUD at lines 354–411. Strict tuple comparison ignores a second operation in the same millisecond from the same node. A future-dated peer record also defeats a local deletion/update until the clock catches up. The probe demonstrated both silent no-op deletion cases.

**Change/verify:** use a monotonic logical/hybrid clock that advances past stored versions for local edits; validate extreme peer times. Test equal-millisecond updates, clock rollback, and a peer one day ahead.

### R21 — Invalid or older settings JSON silently resets every preference

**Inspected.** [store.rs](../src/store.rs), `load_settings` at line 293; [settings.rs](../src/settings.rs), `AppSettings` at line 10. Deserialization errors are discarded and replaced with the complete default object. Several sections and fields are required. A missing field, unknown enum, or damaged blob can reset API configuration and privacy switches, including enabling sync; the next ordinary save overwrites the prior blob.

**Change/verify:** version and migrate settings, default compatible missing fields individually, surface errors, and preserve the original blob. Test historical/minimal configurations and invalid enums without changing unrelated privacy settings.

### R22 — Database migrations lack a coordinated, recoverable boundary

**Design risk.** [store.rs](../src/store.rs), plaintext migration at line 197 and `open` at line 216. Migration uses a shared `.migrating` path and rename without a cross-process lock or backup/recovery protocol. GUI and service can enter it together. The schema sets `user_version=2` before best-effort alterations, ignores alteration errors, and does not reject newer schemas. Wrong encryption keys are also routed through the plaintext-detection path. Existing WAL/active connections need explicit migration tests; data corruption was not demonstrated here.

**Change/verify:** exclusive versioned transactions, distinguish wrong key/plaintext/corruption, preserve a recoverable backup, and fault-test two simultaneous openers, active WAL, disk-full, interrupted migration, and newer schemas.

### R23 — Hotkey cancellation/muting/reconfiguration loses session state

**Inspected.** [hotkey.rs](../src/hotkey.rs), event polling at line 387 and dictation callback at line 654. Escape calls cancellation without clearing the shared toggle or smart-press state, so the next Toggle press can stop a nonexistent recording. Disabling shortcuts skips releases and held-key updates as well as presses. Restarting the listener discards active bindings without cancel/release. A recording may stay armed after mute/reconfiguration; Smart/Toggle have no Push watchdog protection.

**Change/verify:** centralize activation state with session ownership; process releases while muted and cancel active bindings on restart. Test Escape→Toggle, mute while held, and edit bindings during each activation mode.

### R24 — Shortcut capture does not suspend the service listener

**Inspected.** [app_settings.rs](../src/app_settings.rs), persistence/capture paths at lines 820, 1204, 1233, and 1271; [hotkey.rs](../src/hotkey.rs), `apply_from_settings` at line 710. These start a second evdev listener in the GUI with log-only hooks. `hotkey::stop()` affects that process only. The service continues listening while the user records a shortcut, so testing an existing binding can start real dictation. This contradicts the frontend-only ownership model.

**Change/verify:** keep engine listeners in the service and send an explicit capture/suspend request to it. Recording and canceling shortcuts must leave no service dictation or extra GUI evdev listener.

### R25 — Legacy custom shortcuts migrate to F13

**Reproduced.** [settings.rs](../src/settings.rs), `HotkeySettings` at line 280 and `migrate_legacy` at line 339. Missing `dictation_hotkeys` deserializes to the nonempty default F13 list. That list takes precedence over the legacy custom slot, then migration clears the custom value. The probe's legacy `Ctrl+D` becomes F13.

**Change/verify:** distinguish an absent new list from an explicitly configured list and prefer the legacy value only during migration. Add fixtures for old custom shortcuts, default shortcuts, explicit new lists, and empty lists.

### R26 — Shortcut parsing, conflict detection, and inverse key mapping disagree

**Inspected.** [hotkey.rs](../src/hotkey.rs), `token_for_evdev_code` at line 162, `parse_combo` at line 220, and modifier matching. Multiple target keys overwrite the prior target instead of rejecting a combo such as `Ctrl+A+B`. Matching accepts extra modifiers, so nested combos can fire together although exact-equality conflict checks accept them. The inverse map labels evdev 54 (Right Shift) as C and emits punctuation tokens the parser rejects; many letter mappings fall back to layout-derived keyvals.

**Change/verify:** one canonical key map, exactly one target, explicit modifier-match policy, and overlap-aware conflict checking. Exhaustively round-trip supported keycodes and test nested bindings/non-US layouts.

### R27 — Held keys are merged across input devices and survive disconnects

**Inspected.** [hotkey.rs](../src/hotkey.rs), `poll_events` at line 387 and device rescan logic. A single global set represents keys held on several keyboards. A release on one keyboard can clear a modifier still held on another; a vanished device does not remove only its own held keys. This can produce missed/incorrect shortcuts and stuck modifier state.

**Change/verify:** track held keys per device, aggregate counts, and release owned state on disconnect. Use two virtual keyboards with overlapping modifiers and unplug a device during an active binding.

### R28 — Capture and inference failures lack actionable user feedback

**Inspected.** [mic.rs](../src/mic.rs), `ParecMic::build` at line 32 and `MicInput`; [transcribe.rs](../src/transcribe.rs), empty capture at line 2870; [bin/mywhisper.rs](../src/bin/mywhisper.rs), completion handling. A `parec` child that immediately exits is still accepted as an active mic; its liveness is not monitored. Fallback simulated bars can look like listening. Empty audio returns `Ok(None)` and calls `engine_ok()`, while errors mostly go to stderr and the pill hides. `OverlayState::Error` is not wired to these failures.

**Change/verify:** represent real capture availability separately from visual simulation, monitor child/stream health, and surface failed setup/capture/transcription/paste with recovery actions. Test absent mic, invalid source, exited `parec`, missing runtime, and refused server.

### R29 — Whistle FIFO/process finalization has unbounded and incomplete paths

**Inspected.** [transcribe.rs](../src/transcribe.rs), Whistle CLI around line 1494, stream around line 1630, `feed` around line 1695, and `finish` around line 1745. Checking child liveness does not guarantee it has opened the FIFO; blocking write-only open can hang forever. Writes and the file CLI lack deadlines. The stdout reader occupies a pool worker and is not joined before taking its last text. Timeout/nonzero exit with partial text is treated as success, suppressing fallback. Drop kills but does not wait, and these children lack server-style parent-death handling.

**Change/verify:** nonblocking/deadline-aware FIFO startup and IO, cancellable child supervision, drain/join output before success, and distinguish partial results. Test alive-but-never-open, blocked reader, final line at exit, crash, and service death.

### R30 — Speculative finalization can drop unprocessed tail audio

**Inspected.** [transcribe.rs](../src/transcribe.rs), `finish_session` around line 2868 calls `Recorder::finish()` before stopping/joining the speculative worker. `finish()` takes the recorder's samples. The worker's final snapshot is then empty, so samples received since its previous tick do not reach its buffer. The final decode uses that incomplete buffer instead of the full `pcm` already taken.

**Change/verify:** stop the speculative loop and provide the complete settled buffer before consuming it; retain a full-decode fallback. Feed speech only between the final speculative tick and release and assert it appears in the final decode input.

### R31 — Speculative agreement confuses rolling-window and full-prefix text

**Inspected algorithm defect.** [transcribe.rs](../src/transcribe.rs), `run_spec` around line 2188; [speculative.rs](../src/speculative.rs). Each hypothesis is for a moving unconfirmed window, but confirmed window text replaces the entire committed string and its word count is compared with the entire previous commitment. Window shifts invalidate that comparison and can stall/drop prior commitments. The commit sample index is a fixed audio heuristic rather than a word boundary. The documented full-decode fallback on failed merge is not implemented.

**Change/verify:** track text/audio origins for each window, append newly confirmed content, and fall back to full decoding when agreement is uncertain. Test deterministic multi-window hypotheses with punctuation changes, long speech, and no overlap.

### R32 — Delivery is outside the busy guard and can overlap or reorder

**Inspected.** [transcribe.rs](../src/transcribe.rs), worker around line 2764 clears `BUSY` before `deliver`, which performs potentially slow LLM cleanup and clipboard operations. A later utterance can finish/paste first. Concurrent clipboard save/copy/restore sequences can overwrite each other's output or the user's new clipboard. Duration is measured at finalization, including flush/queue delay, while its start is recorded after session setup; usage statistics therefore do not consistently represent speaking time.

**Change/verify:** ordered delivery with its own ownership/queue and capture-edge duration timestamps. Force the first cleanup to be slower than the second and check paste/history order and clipboard state.

### R33 — Capture memory and streaming copying are unbounded

**Inspected.** [audio.rs](../src/audio.rs), `Recorder::push` at line 41 and `snapshot` at line 57; streaming/speculative pumps in [transcribe.rs](../src/transcribe.rs). All samples and slice stamps remain in RAM for the entire utterance, even after sending chunks. Frequent snapshots clone the entire growing buffer, producing quadratic total copying. Toggle/Smart recording can continue indefinitely. CPAL callbacks allocate and contend on these mutexes on the real-time audio thread.

**Change/verify:** explicit duration/size limits, incremental/ring-buffer reads with a bounded fallback policy, and minimal callback work. Benchmark long synthetic recordings and cancellation under slow inference with memory/latency limits.

### R34 — Server readiness and keep-warm ownership are insufficient

**Inspected.** [transcribe.rs](../src/transcribe.rs), `ensure_server` at line 696 and reaper around line 631. A child is placed in the shared map before readiness/probe completes; concurrent ensure calls return success based on liveness/key alone. This lets inference race startup probing. Last-use timestamps are refreshed by ensure, with no active-session lease, so a long recording can outlive the keep-warm timeout and lose its server. Failed startup leaves a live cached entry eligible for reuse.

**Change/verify:** explicit Starting/Ready/Failed states and session leases; all callers await the same readiness result. Test concurrent cold start, failed probe, and recording longer than the smallest timeout.

### R35 — Cohere is selectable/downloadable but cannot be used

**Inspected.** [transcribe.rs](../src/transcribe.rs), `local_plan` at line 351, always rejects Cohere. The registry and UI allow selecting/downloading its large model, and CHANGELOG language support suggests it is operational. Even a fully downloaded configuration cannot create a plan.

**Change/verify:** implement a supported runtime or disable selection/download with a clear explanation before users spend bandwidth/storage. A supported advertised model must pass an actual plan and inference smoke test.

### R36 — Self-hosted WebSocket configuration drops authentication and accepts incompatible routing

**Inspected.** [transcribe.rs](../src/transcribe.rs), `plans_for` at line 193 and `Plan::SelfHostedWs` at line 124. A WS plan retains only the URL, discarding the saved API key; connection construction adds no bearer header. README advertises optional authentication for self-hosted protocols. Disabling streaming with a WS URL instead creates an HTTP-compatible plan whose base is `ws://`/`wss://`, which an HTTP request cannot use. An existing test asserts this routing rather than rejecting it.

**Change/verify:** carry auth into upgrade headers and validate URL/protocol combinations. Test a loopback WS server requiring Bearer authorization, plus streaming toggles for each accepted URL scheme.

### R37 — Claude cleanup thinking requests are invalid and response parsing misses text

**Inspected and checked against provider documentation.** [transcribe.rs](../src/transcribe.rs), Claude branch at line 3266 sets `max_tokens=1024` and `budget_tokens=10000`, contrary to the [manual thinking budget rules](https://platform.claude.com/docs/en/build-with-claude/extended-thinking). The configured default Haiku 5.5 also requires newer thinking configuration according to that page. Parsing only `/content/0/text` ignores final text when the first block is thinking. No paid API call was made.

**Change/verify:** model-capability-aware thinking configuration and extraction of text blocks by type. Use request/response fixtures and a live disposable credential smoke test for each supported thinking mode.

### R38 — Cleanup accepts truncated or noncanonical replies as complete dictation

**Inspected.** [transcribe.rs](../src/transcribe.rs), cleanup providers around lines 3180–3320. Claude's fixed 1024-token ceiling can truncate long speech; termination reasons are not checked. Other providers' completion/finish reasons are also ignored. Google concatenates every text part without excluding thought parts if returned. The cleanup prompt permits intentional empty output, but `post_process` treats empty output as provider failure and eventually pastes the original text.

**Change/verify:** distinguish complete, partial, empty-by-policy, and failed responses; extract canonical answer blocks only. Test long transcripts, token-limit replies, thought blocks, and filler-only input.

### R39 — Audio retention differs by engine and ignores a changed privacy preference

**Inspected.** [transcribe.rs](../src/transcribe.rs), streaming return paths at lines 2828–2864 and audio write around line 3409. Streaming utterances return no WAV despite the whole recorder being available, so Keep audio silently excludes successful streaming/Whistle runs. File retention snapshots the switch at press; `deliver` can still write the provided WAV after the user has turned audio retention off, because it does not recheck that flag before writing.

**Change/verify:** apply one documented retention policy across engines and recheck privacy immediately before persistence. Test streaming/file success and switching audio retention off while finalization is delayed.

### R40 — Retained audio and temporary files are not protected like the database

**Inspected.** [transcribe.rs](../src/transcribe.rs), audio vault write around line 3418, keyword file around line 1463, Whistle staging, and `server_logs` at line 822. WAV/keyword/log creation relies on process umask rather than private creation. With a common 022 umask, files can be readable by other local users where parent directories permit it. Shared predictable `/tmp` and `/dev/shm` paths also allow collisions/symlink interference. SQLCipher protects `state.db`, not these artifacts.

**Change/verify:** private directories/files from creation, unpredictable securely created staging paths, cleanup on all outcomes, and explicit vault encryption policy. Test permissions under umask 022 and preexisting conflicting paths using disposable locations.

### R41 — Retention maintenance only runs during successful delivery

**Inspected.** [transcribe.rs](../src/transcribe.rs), `deliver` at line 3388. Old history/audio are purged only when another transcript is delivered. Audio purge additionally requires Keep audio to remain enabled; turning it off leaves prior audio indefinitely unless manually cleared. History deletion/clear does not delete its corresponding WAV. Thus the UI's retention periods are not enforced while idle, and transcript deletion does not clear related audio.

**Change/verify:** periodic/startup retention maintenance and a clear per-transcript audio-deletion contract. Advance time with no new dictations and with recording disabled; expired files/text must be handled as promised.

### R42 — Sync restart and address/discovery handling are unreliable

**Inspected.** [service.rs](../src/service.rs), stop/start at lines 203–258; [net.rs](../src/net.rs), dialing at line 480, mDNS around line 680, and `watch_discovered` at line 811. Stopping sets a flag but does not join the accept loop before rebinding the same port; an immediate restart can fail, after which the watcher records the configuration as applied and waits for another edit. DNS/handshake errors on the first address short-circuit remaining candidates, and only the first resolved address is tried. IPv6 mDNS addresses lack brackets. The service appearance watcher does not persist newly discovered dial addresses; only the GUI's Nearby refresh does. Removed services remain in discovery, preventing reliable reappearance detection.

**Change/verify:** join listener shutdown, retry failed desired state with backoff, try all usable addresses, format IPv6 correctly, and update/expire discovery in the engine. Test rename/restart at a fixed port, stale first address, IPv6, DHCP changes without GUI, and disappear/reappear.

### R43 — Pairing and gossip need durable revocation and bounded reachability metadata

**Inspected.** [store.rs](../src/store.rs), `decide_request` around line 1026, `remove_peer` at line 950, and `apply_gossip` at line 1067; [net.rs](../src/net.rs), exchange contact bookkeeping around line 369. Accept/manual trust automatically grants introducer authority. Removing/declining deletes the row, so an introducer can add the device back as trusted. There is no durable reject/revoke marker or UI choice for introduction authority. Outgoing sync sends no listening addresses; the responder can record the dialer's ephemeral source port as a dial target. Addresses accumulate without expiry, and stale gossip overwrites custom names without versions.

**Change/verify:** explicit introduction policy, durable local revocations, listener-only advertised addresses, bounded/expiring address metadata, and versioned names. Test unpair→gossip, asymmetric pairing without mDNS, and repeated connections.

### R44 — Full-state synchronization scales poorly and applies partial rounds

**Inspected/design risk.** [net.rs](../src/net.rs), payload export/merge around lines 415–450; [store.rs](../src/store.rs), export/apply methods. Every round exports all words/snippets/history/tombstones and applies records one at a time, even when unchanged. Multiple round triggers can run concurrently. Cross-table application has no transaction, so a failed round can partly commit. Permanently retained rows make the 256 MiB message cap an eventual hard failure. No negotiated schema/features accompany the single protocol version.

**Change/verify:** coalesce per-peer rounds, batch transactional merges, delta/change tracking and a tombstone lifecycle; explicitly version compatibility. Benchmark a large history over a slow link and inject failure halfway through a merge.

### R45 — Clipboard copy reports success before the command succeeds; restoration is incomplete

**Inspected.** [clipboard.rs](../src/clipboard.rs), copy at line 176, reaping at line 202, and pipeline at line 287. After writing stdin, copy returns success without checking the child status or selection readiness. A failed `wl-copy` can therefore be followed by pasting the old clipboard. If the previous clipboard was empty/unavailable, Keep in clipboard off has nothing to restore, so the transcript remains. Fixed 120/250 ms sleeps are not proof of selection consumption; a delayed target or a user clipboard change can be overwritten.

**Change/verify:** wait for successful selection ownership with a deadline, represent empty clipboard explicitly, and restore only while still owning the selection. Use fake CLI fixtures for failure/readiness plus a slow Wayland target and user copy during delivery.

### R46 — GTK signal closures retain dictionary widgets after removal

**Reproduced.** [app_dictionary.rs](../src/app_dictionary.rs), `word_card` at line 96 and analogous snippet callbacks. A card owns its buttons, while button signal closures strongly capture the card. Removing the card/page does not break the cycle. The probe dropped the dictionary page and verified the card's weak reference still upgraded. Periodic rebuilds after data-generation changes and language rebuilds can accumulate retained cards and app-state references. Similar strong widget captures merit review in forms/chain lists.

**Change/verify:** weak captures or explicit signal disposal; avoid rebuilding unchanged records. Repeated refresh/edit/navigation/language cycles must release removed widgets and keep memory stable.

### R47 — Opening Settings changes the active remote engine's language

**Reproduced.** [app_settings.rs](../src/app_settings.rs), `stt_lang` at line 1963 and refresh around line 2823. The global transcription-language list follows the local model even if the local option is absent/disabled and the active chain is entirely remote. Rendering the dropdown corrects unsupported values and persists that correction. A Spanish remote-only configuration becomes English merely by opening Settings when the remembered local model is English-only.

**Change/verify:** provider-specific language configuration or a remote-capable global list; rendering must not silently mutate operational settings. Test remote-only/non-English chains with a different local model remembered.

### R48 — Release packaging and prerequisite handling are incomplete

**Inspected; version failure reproduced.** [packaging/nixos/flake.nix](../packaging/nixos/flake.nix), lines 17–34, has a placeholder hash. After correcting it, `fetchzip`'s default root stripping conflicts with the extra inner-directory search in `installPhase` ([Nixpkgs fetcher documentation](https://github.com/NixOS/nixpkgs/blob/master/doc/build-helpers/fetchers.chapter.md)). Arch/Nix dependencies omit default delivery tools `wl-clipboard`/`wtype` and capture/discovery tools `parec`/`pactl`; the README's system dependency list is incomplete. Release binaries require GTK 4.22 through Cargo features, while package constraints do not express a supported ABI baseline. [packaging/arch/update.sh](../packaging/arch/update.sh), line 27, uses `enable --now` without restarting an already active engine, leaving old code running after an upgrade. [bin/mywhisper.rs](../src/bin/mywhisper.rs), line 18, initializes DB/keyring/identity before processing `--version`; the release binary panics without Secret Service instead of printing its version.

**Change/verify:** make advertised packages build/install unmodified, supply or clearly require runtime tools, define/test the ABI baseline, restart the upgraded service, and parse informational CLI flags before initialization. Test clean installation and upgrade in disposable Arch/NixOS desktop environments with and without prerequisites.

### R49 — Download cancellation, integrity, and installation are incomplete

**Inspected.** [stt.rs](../src/stt.rs), `download_model` at line 448 and runtime downloaders at lines 898, 1166, and 1329. HTTP clients disable the overall timeout and do not set read/connect deadlines; cancel is checked only between blocking reads, so an accepted but stalled response cannot be canceled promptly. Models use mutable upstream URLs and verify only a lower size bound or file presence, while runtime binaries do have SHA-256 verification. Model/archive extraction writes into the final installed directory; failure can leave partial required files that presence-only installation checks accept. Delete/reset/cache-clear can race active download/extraction because there is no shared install lock or coordinated cancellation. A failed/retried partial extraction is not an atomic replacement.

**Change/verify:** bounded/cancellable network IO, pinned model hashes or explicit integrity validation, private staging plus atomic promotion, and shared ownership for download/delete/reset. Test a response that stalls after headers, canceled extraction, disk-full, malformed model, retry after partial install, and deletion during a download. Validate the actual pinned upstream assets in release integration checks; this audit did not download all multi-gigabyte models.

### R50 — Settings/state boundaries and documented architecture have drifted

**Inspected.** [app.rs](../src/app.rs), state load at line 55; [app_settings.rs](../src/app_settings.rs), preference persistence; [store.rs](../src/store.rs), `save_settings` at line 332. The GUI holds and writes a whole settings snapshot rather than atomically updating individual preferences, so another frontend's changes can be overwritten. Dictionary content is duplicated in that JSON despite typed tables being authoritative, leaving stale sensitive snippet expansions in the blob until another save. Store opening runs schema/migration/update work repeatedly from watchers; even unchanged sync merges bump generation and rebuild widgets. `checkpoint` has no caller despite comments promising clean-shutdown external-file replication. Comments describe GUI engine ownership, a future P2P layer, user-placed Needle, no audio retention, and pool-only background work that no longer match runtime behavior. Many direct thread spawns remain outside the bounded runtime, and GTK callbacks synchronously perform microphone discovery, media D-Bus operations, process probes, and database work. These compound the lifecycle/performance problems above.

**Change/verify:** establish clear repository/service/session/provider boundaries, transactional preference patches, one authoritative dictionary store, read-only polling, explicit task ownership and cleanup, and accurate support docs. Test two frontend snapshots, startup/rebuild latency with slow external services, and any promised backup/export workflow. Remove obsolete helpers/comments rather than treating them as functioning features.

## P3 — release hygiene and maintainability

### R51 — Release automation has no stable-version gates or artifact integrity checks

**Inspected/check failures reproduced.** [.github/workflows/release.yml](../.github/workflows/release.yml) builds and publishes arbitrary `v*` tags without tests, fmt, Clippy, dependency audit, `--locked`, or tag/package version comparison. The current package/Arch/Nix versions are 0.9.2; tagging 1.0.0 alone can publish a tarball whose binary still identifies as 0.9.2. Arch uses `sha256sums=('SKIP')`. Release creation does not mark prerelease tags as prereleases, and update paths select the first release without an explicit stable-only policy. The fallback changelog compares the tag with itself. Existing fmt/Clippy failures are mainly hygiene, not 88 independent functional bugs. No project LICENSE/third-party notice artifact ships despite source files describing MIT-ported code; resolve the intended license and preserve applicable notices before distribution.

**Change/verify:** require release checks and version consistency, publish/check hashes, define stable/prerelease selection, generate meaningful release notes, and include licensing metadata/notices. Dry-run a candidate release and verify the unpacked artifact and package manifests.

## Recommended release sequence

1. **Privacy and replicated state:** R01–R07, R17, R19–R22, R39–R44. Establish exact off/delete/reset/revocation semantics first; changes here affect migrations and the wire format.
2. **Recording/session ownership:** R08–R16, R18, R23–R34. A generation-owned session should own its recorder, cancellation token, tasks, child handles, readiness, and finalization. All exits need bounded completion and cleanup.
3. **User-visible provider and UI behavior:** R28, R35–R38, R45–R47. Unsupported features should be disabled or clearly explained; failures must be visible instead of looking like successful empty dictation.
4. **Distribution and release gates:** R48–R51. Validate the exact shipped artifact in clean supported environments, then bump versions together and tag only that verified commit.

## Missing release acceptance coverage

Convert the demonstrated failures into regressions that assert the corrected behavior; the bundled probes currently assert the bugs and are audit evidence, not a future passing CI suite. Add deterministic fake-provider/microphone/clock fixtures so privacy, cancellation, response parsing, and scheduling can be exercised without credentials or large models. Include record-merge permutation/concurrency tests, repeated task cancellation, network stall/resource bounds, migration interruption, multi-device hotkeys, and clipboard ownership tests.

The actual candidate still needs end-to-end speech on every advertised local runtime/provider, model/runtime download/cancel/retry, real WSS with auth and timeouts, supported compositor keyboard/paste behavior, mono/stereo/Bluetooth and device unplug/reconnect, actual two-device pairing/revocation/discovery, private vault/keyring handling, and clean package install/upgrade. Passing UI smoke and pure unit tests alone cannot certify those integrations.
