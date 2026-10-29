# Independent review of fix/release-audit

Verdict: **DO NOT MERGE as a completed release-audit fix.** There are 12 actionable findings below: ten independently reproduced and two confirmed by tracing the implementation. Passing checks are substantial progress, but do not establish closure of the original 51 audit groups.

Reviewed repository: GitHub `aaronroquefonseca/MyWhisper`.
Reviewed head: `fix/release-audit`, **f426c9ae71854c262a43f6bd337036d82173adfc**, authored by Aaron Roque Fonseca on 2026-10-10.
Intended target: `main`, fetched at **15776960fe4351be551df607861db21b59d0e388** (`chore: bump version to 0.9.3`).
Intent: fix the pre-stable release audit in one PR; no PR currently exists. The branch's own handoff explicitly describes unfinished implementation and marks all 51 grouped findings partial.

Compared with the immutable audit commit `f018ab855af41759e275c80708d67973f2890587`: 111 files, +23,967/-3,706 lines. Compared with the common ancestor of main: 127 files, +27,003/-3,706 lines, including the original audit evidence. This is a large change spanning storage/migrations, privacy and authentication, capture/runtime ownership, sync, downloads, GTK UI, packaging/release infrastructure, and a vendored Secret Service patch.

Independent scores (10 is best): **Risks 3/10; Functionality 4/10; Code quality 6/10.** Privacy boundaries and recording reliability still fail adversarial cases. The new ownership helpers and regressions materially improve quality; large modules, duplicated lifecycle decisions and incomplete migration/cancellation boundaries remain.

Review was performed in detached worktree `/tmp/mywhisper-review-f426c9a`. No tracked implementation files were changed; no commit, push, PR, approval or merge was performed. All probes used synthetic content, temporary databases/files, loopback endpoints or private desktop sessions. No cloud credentials were used.

## Findings and required corrections

### F01 — P1: canceled/reset recordings are uploaded to the next provider

References: [finish_session fallback loop](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L3295), [outer finalization cancellation checks](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L3115), [cleanup fallback loop](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L3534).

Reproduced with two loopback HTTP transcription endpoints. The primary receives the WAV and waits at a barrier. Call `cancel_utterance`, or commit `Store::reset_all_data`, then let the primary return HTTP 500. The fallback receives another 32,255-byte request body containing the same recording after the cancellation/reset. Finalization notices cancellation only after `finish_session` returns. Reset similarly has no current-policy check between provider attempts. The cleanup chain has the same missing propagation: cancellation is checked outside `post_process`, rather than before each cleanup request.

Blocking HTTP/WS/CLI operations also keep running after cancellation, retaining recordings and the single ordered finalization worker for their timeout (HTTP inference timeout is 180 seconds per attempt). The service's two-second privacy watcher cannot prevent a subsequent attempt inside this loop, even once it has set the token.

Required fix: give capture, STT attempts, cleanup attempts, connection setup and active IO a common cancellation/policy owner. Check durable capture/reset epoch before each external disclosure, cancel blocked operations promptly, and stop fallback traversal when authorization ends. Preserve ordered delivery without letting canceled IO monopolize it.

Acceptance: block the first provider at headers and at response-body IO; cancel/reset/change policy from a second connection/process; assert no second provider or cleanup request begins, no later clipboard/history/audio output appears, and task counts and finalization admission recover promptly. Test WS/TLS and CLI equivalents.

Evidence: `probe-cancel.log`, `probe-reset.log`; modes `cancel` and `reset` in the independent probe.

### F02 — P1: compatible-looking partial settings silently enable history and sync

References: [AppSettings serde default](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/settings.rs#L9), [load_settings](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/store.rs#L513), [sync defaults](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/settings.rs#L47).

Reproduced in an encrypted disposable database: save history=false/sync=false, remove those sections from the stored JSON while retaining valid JSON and the remaining settings, then reopen. Both settings become true. Deserialization succeeds, so the new malformed-JSON fail-closed branch is bypassed. A stored read failure is also swallowed by `.ok()` and treated as no settings. A normal later save can make the permissive defaults durable. Existing peer trust makes silently enabling sync material.

Required fix: distinguish a genuinely new configuration from an existing incomplete/failed-to-read configuration. Use explicit versioned migrations and conservative defaults for absent privacy fields; propagate database read failures. Preserve explicit settings, original recoverable JSON and unknown fields where appropriate, and visibly explain recovery.

Acceptance: remove whole sections and individual fields from historical/current configurations, inject settings read failure and unknown enums, and prove no previously restrictive privacy/sync choice silently becomes permission. Fresh-install defaults can remain a separately documented policy.

Evidence: `probe-settings.log`, mode `settings`.

### F03 — P1: queue pressure silently loses an entire valid dictation

References: [queued press handling](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/bin/mywhisper.rs#L277), [reserve_utterance](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L2449), [release with no live session](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L3013).

Reproduced by occupying the four short-job workers, reserving and queuing a press exactly as the service does, feeding one second of synthetic PCM, holding for more than the 0.3-second minimum, then releasing before press-arm executes. The completion is `Cancelled`, although the user never canceled. Once the queue drains, arming is invalidated and no recording remains. Epoch ownership correctly prevents the old orphan-capture bug, but reserving an epoch does not actually arm the recorder. A partial delay also clips the beginning of a longer utterance.

Required fix: capture ownership and the recorder must start at the input edge without waiting for settings/keyring/plan/server work. Carry timestamps and session ownership through queued preparation; preserve raw capture until preparation can complete. If admission genuinely fails, show a failure rather than a successful/canceled disappearance.

Acceptance: deterministically block all workers, press/speak/release, unblock and verify the exact captured samples and one completion; test canceled and repeated press/release sequences, delayed settings IO and nonzero release delay.

Evidence: `probe-queued.log`, mode `queued`.

### F04 — P2: arming holds a lock needed by the GTK release/cancel handlers across IO

References: [ARM_LOCK around database open](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L2540), [release takes the same lock](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L3013), [cancel takes the same lock](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L2861).

Reproduced by holding the disposable database initialization lock, starting background arming, then releasing. The release caller blocks for approximately 1.016 seconds until the test releases the file lock. The real initialization/keyring paths can wait much longer. Moving arming to a worker does not make GTK nonblocking when that worker owns a lock needed by GTK.

Required fix: perform fallible/blocking IO outside the small ownership critical section, then validate the epoch before publishing the result. Release/cancel must promptly invalidate an in-progress arm without waiting for its IO.

Acceptance: stall initialization, keyring and settings reads; assert press/release/cancel handlers return within a short bound and neither a late recorder nor late preview is published.

Evidence: `probe-armlock.log`, mode `armlock`.

### F05 — P2: warming a new configuration kills a server still transcribing the previous recording

References: [ensure_server replacement](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L664), [new-utterance warmup](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L2580).

Reproduced with a disposable mock whisper-server using the real supervisor and finalization paths: block the original `/inference`, warm a plan with a changed language, and observe `whisper key changed; replacing`. The new warmup succeeds while the original finalization becomes `Failed` because its server was killed. Active-session/finalization counters protect the keep-warm reaper, but do not protect replacement or the single-server rule.

Required fix: lease a supervised engine to active work. Serialize/defer configuration replacement until its users finish, or explicitly cancel their owners and report the outcome. Prevent a later warmup from destroying an earlier accepted recording.

Acceptance: block inference and change language, model, backend, bind address and port while recording/finalizing another utterance. Assert the earlier recording completes under its snapshot or receives an explicit supported cancellation outcome; no incidental connection failure or stale server-ready/model status.

Evidence: `probe-server-replacement.log`, mode `server-replacement`, `fake_whisper.py`.

### F06 — P2: discarded-history merges still do not converge and retain speech in one order

Reference: [history conflict update](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/store.rs#L1317).

Reproduced with a normal row containing text/raw_text/words and an empty discarded row with the same UUID. Normal-then-discarded keeps the old speech and word count; discarded-then-normal keeps empty speech and zero words. Both rows are discarded, but their exported payloads differ. The OR fix correctly prevents deletion-flag resurrection; it does not normalize discarded content. The current permutation regression always includes a deleted snapshot, which eventually redacts everything and hides this two-state failure.

Required fix: canonicalize discarded rows on insertion and conflict update, clearing speech and word counts whenever either side makes the record discarded. Preserve the explicitly supported discarded-marker duration policy; do not accidentally erase legitimate marker duration.

Acceptance: all normal/discarded-only permutations and repetitions, with and without later deletion, converge to the same payload. Repeat through encrypted sync and inspect exports for discarded speech.

Evidence: `probe-discard.log`, mode `discard`, ending with `Converged: false`.

### F07 — P2: the 16 MiB cap makes larger histories permanently unsynchronizable

References: [CONTENT_CAP](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/net.rs#L48), [full snapshot exchange](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/net.rs#L660), [send_msg cap](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/net.rs#L377).

Reproduced through an authenticated, mutually trusted encrypted loopback exchange with an approximately 18.2 MB synthetic history payload. The initiator fails with `message too large`; the responder gets `connection closed during read`. Full history, raw/final text, dictionary, snippets and tombstones are serialized into one message. There is no pagination/cursor or delta protocol, so an ordinary accumulated history can eventually stop all content synchronization, including small new changes and deletions. Export/serialization also happen before this limit is checked.

Required fix: bound database reads and serialized batches, negotiate compatible paging/delta behavior and retain convergence/authorization across pages. Do not fix this solely by raising/removing the cap.

Acceptance: total state larger than the cap with individually bounded records, interrupted/resumed pages, concurrent edits/deletions/revocation, restart and old-peer negotiation; assert convergence with bounded memory and useful progress/errors.

Evidence: `probe-oversized.log`, mode `oversized`.

### F08 — P2: WebSocket DNS runs outside the startup deadline and cancellation

Reference: [blocking resolver before connect](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L1288).

Reproduced by interposing a stalled NSS/getaddrinfo implementation for one synthetic hostname in the probe process only. The advertised ten-second startup deadline returns after 11.000 seconds because `to_socket_addrs()` blocks before a cancellable/deadline-controlled operation exists. The socket shutdown timer is created only after resolution and connection succeed. A permanently stalled resolver can retain a session task indefinitely and exhaust the eight-task process budget after repeated attempts.

Required fix: reuse a bounded owned resolver such as the new sync resolver, or another actually cancellable/deadline-enforced mechanism. Propagate session cancellation through resolution, connect, TLS/upgrade and ongoing IO.

Acceptance: stalled NSS, multiple resolved addresses, expired deadline, cancel during resolution/connect/upgrade and repeated cancellations; assert bounded elapsed time and no leaked task capacity.

Evidence: `probe-dns.log`, mode `dns`, `stalled_dns.c`/`stalled_dns.so`. The hook intentionally waits eleven seconds and affects only `mywhisper-review.invalid`.

### F09 — P2: corrupt legacy model installations are still accepted as installed/usable

References: [is_installed](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/stt.rs#L431), [plan file checks](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L299), [required transducer files](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L395).

Reproduced with four zero-byte Parakeet files: `is_installed=true` and `plan_for=Ok(sherpa-offline)`. A Whisper file truncated to 96% of the pinned length is also reported installed because the check accepts >=95%. The new downloader verifies fresh transfers and promotes atomically, but previously partial/corrupt installs are not repaired or rejected. Presence-only runtime checks have the same weakness. Users can be shown an installed model that fails every dictation without a clear repair state.

Required fix: validate legacy assets and persist a trustworthy installation/validation receipt for future fast checks; verify exact expected size and appropriate content/format/hash in a bounded background path. Preserve deliberate manual-install support with explicit validation and repair behavior. Avoid hashing multi-GB files in GTK polls.

Acceptance: empty/truncated/same-size corrupt models, missing libraries, valid legacy/manual installs, successful repair, corrupt receipt and interruption during validation/promotion.

Evidence: `probe-installed.log`, mode `installed`.

### F10 — P2: Ollama's explicit model-default thinking option becomes forced-off on reload

References: [unconditional migration on every load](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/store.rs#L540), [UI explicitly saves None](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/app_settings.rs#L4335).

Reproduced by selecting/persisting `thinking=None` on an Ollama entry and reloading settings. It becomes `Some(false)`. The loader treats an explicit current user choice like an old missing field on every load; actual requests consequently include `think:false` rather than omitting it. This existing defect remains in the reviewed final tree despite the UI/provider contract work.

Required fix: make the historical migration versioned and one-time, distinguishing missing legacy fields from explicit model-default/null. Preserve all three current choices across persistence and the service request builder.

Acceptance: round-trip None/true/false through GUI save, service load and request capture; migrate a missing legacy field once without overwriting later choices.

Evidence: `probe-thinking.log`, mode `thinking`.

### F11 — P2: persisted hotkey mute is ignored during the initial service startup window

References: [initial listener starts from bindings only](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/bin/mywhisper.rs#L164), [global listening default true](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/transcribe.rs#L438), [watcher sleeps before first application](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/service.rs#L319), [preference application](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/service.rs#L385).

Code-path finding, not an independent full service/input reproduction. `run_headless` starts the hotkey listener without applying `initial.hotkeys_enabled`. The global gate starts true; the watcher first sleeps two seconds and performs database work before applying the saved gate. A service restart therefore enables shortcuts briefly despite persisted mute. Sounds/media-pause preferences similarly wait for that first watcher pass.

Required fix: apply all initial input/feedback preferences before publishing the listener and accepting events; make the first watcher pass immediate. Reconfiguration must preserve correct mute/cancel ownership.

Acceptance: start a private service with saved mute and emit a grabbed private virtual-keyboard shortcut before the first watcher interval. Assert no capture or disclosure. Exercise saved sounds/media-pause preferences without touching host players.

### F12 — P2: Reset All still performs blocking storage/filesystem cleanup on GTK

References: [reset confirmation callback](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/app_settings.rs#L5035), [cache clearing](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/stt.rs#L468), [reset file lock](https://github.com/aaronroquefonseca/MyWhisper/blob/f426c9ae71854c262a43f6bd337036d82173adfc/src/artifact.rs#L185).

Code-path finding, not a timed full GTK reset reproduction. The confirmation callback directly commits the database reset, waits for cross-process installer locks, recursively deletes models/runtimes and clears audio. These operations can take seconds or reach ten-second lock waits and freeze GTK. Other delete/cache controls correctly use `file_action`, so Reset is an important exception. If filesystem cleanup fails after the DB commit, the callback returns before reloading the in-memory settings and rebuilding the UI, leaving it showing pre-reset state although durable state has already reset. Reset also has no explicit supervised-server cleanup command; that original R05 obligation remains unresolved.

Required fix: own reset as a bounded background operation with visible pending/completion/error state, coordinate service/capture/installer/server shutdown, always reconcile UI with committed state, and make interrupted filesystem cleanup recoverable.

Acceptance: active cross-process installer, stalled database/FS lock, large cache, partial deletion failure, process interruption and retry. GTK remains responsive; reset is not reported complete while old owned resources remain; partial failure displays the actual committed state.

## Mechanical merge/release gates

- **Conflict:** `git merge-tree --write-tree origin/main HEAD` returns a content conflict in `packaging/nixos/flake.nix`. Cargo manifests/lock and Arch packaging auto-merge, but version/checksum consistency must be rechecked after updating from main's 0.9.3 bump. Do not blindly retain a v0.9.2 checksum alongside a merged 0.9.3 pkgver.
- **Out of date:** one commit behind main, twelve commits ahead (eleven implementation/handoff commits plus the original audit commit).
- **PR state:** no open PR; draft state, labels, milestone, requested reviewers, approvals and discussion threads are not applicable yet. No claims about required approval policy can be made from an absent PR.
- **Hosted CI:** `gh run list --branch fix/release-audit` returns no runs. The workflow runs on PRs/main/tags; local success does not substitute for checks on the final PR merge commit.
- **Release license hold:** candidate staging correctly reports `project_license_established=false`; `--require-license` refuses publication. The handoff records the owner decision as deferred. This is a publication gate, not permission to invent a project license or copyright holder.
- **Remaining integration evidence:** no independent live cloud-provider/WSS/physical-device reconnect/clean NixOS desktop/running installed-service upgrade validation in this review. Existing real-runtime/packaging evidence is historical and configuration-specific. Five tests remain opt-in/ignored. The previously reported intermittent GTK focus critical did not reproduce in the two fresh smoke runs; that does not close its investigation. Final matrix closures must identify the exact source/artifact tested and remaining limitations honestly.

## Independent validation at the reviewed head

Passed:

- `cargo fmt --all -- --check` and `git diff --check` (also repeated after removing temporary probe sources).
- `cargo clippy --locked --all-targets -- -D warnings`.
- `cargo test --locked --all-targets`: 165 passed, 5 ignored, no failures.
- `cargo build --release --locked --bin mywhisper`.
- `python3 packaging/test-release.py`: 9 passed; `python3 packaging/test-upgrade.py`: 5 passed.
- Current RustSec `cargo-audit audit --deny warnings --json`: zero vulnerabilities, zero warnings.
- Native GTK/Xvfb light-English and dark-Spanish smoke with `G_DEBUG=fatal-criticals`.
- Real CPAL/ALSA synthetic capture: F32/I16/U16, each with 1/2/6 channels at 48 kHz, nine configurations.
- Private real Secret Service: eight simultaneous creator processes, shared identity/key, missing-key refusal/recovery, duplicate candidate recovery, locked-keyring refusal and stalled-keyring five-second deadlines.
- Private sync lifecycle: fixed-port replacement, stalled-socket cleanup, joined watchers, outbound-only encrypted catch-up and mutual mDNS discovery/goodbye.
- Private nested niri/wtype clipboard gate: delayed consumption, empty/text/binary restoration, user replacement, cancellation, copy-only and no-op.
- Required grabbed private virtual-keyboard hotkey gate: six selected tests passed.
- Release candidate staging at exact reviewed SHA, `source_dirty=false`, all 370 reachable dependency notice entries complete. Archive layout and SHA-256 verified. Candidate version is 0.9.2 because this is the reviewed branch, not the merged main version.

Candidate SHA-256: `3575d9d4500d042ba3a4f6d3fc37dcbb03d4a0e12a9407012f67bbaef775321b`.
Review probe executable SHA-256: `b6b4f41f129f876f46346d7da98dd185cc6a7585daccdf829d0513959c914828`.

## Reproduction artifacts

Everything in this directory is local review output, not committed or pushed. The findings above are self-contained and source links refer to the immutable pushed implementation commit.

- `review_acceptance.rs`: source of the independent probes; temporarily compiled as an example in the detached worktree and then removed from the repository.
- `review_acceptance`: saved compiled executable for the reviewed implementation.
- `fake_whisper.py`: private synthetic server used by the supervisor replacement probe.
- `stalled_dns.c` and `stalled_dns.so`: single-hostname resolver stall injection; use only on the review probe process.
- `probe-*.log`: reproductions described above; `cargo-test.log`, `clippy.log`, `release-build.log`, `audit.json`, `ui-*.log`, `*-smoke.log` and candidate output preserve independent validation.

To rerun a probe, execute the saved binary with one of `cancel`, `reset`, `settings`, `queued`, `armlock`, `server-replacement`, `discard`, `oversized`, `installed` or `thinking`, with `MYWHISPER_DB_KEY` set to a disposable 64-character hex value. The probe uses temporary state and synthetic loopback endpoints; do not repurpose it for real data. `dns` additionally sets `LD_PRELOAD` to the saved single-hostname shim in that command only. Source rebuild of `server-replacement` uses `include_str!` pointing at the accompanying fake server's current absolute path; adjust that path when relocating the artifacts.

All findings must receive meaningful regressions that fail against this head, fixes that preserve supported behavior and data, and fresh final validation after updating from main. Do not count the ten adversarial probes as passing feature tests: their recorded output demonstrates the defects.
