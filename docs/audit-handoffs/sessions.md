# fix(capture): own sessions, provider IO and engine lifetimes

Draft scope: R08, R09, R10, R11, R13, R15, R17, R18, R28, R29, R30, R31, R34, R35, R36, R39, R40, R50.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-clipboard-ownership`.

Largest remaining scope; live WSS/cloud/stress and packaged inference pending. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Continuation (this head): fix two extraction regressions without weakening
assertions. R28 overlay show_error needs tr("capture.failed"); the entry was
dropped with i18n.rs outside the extraction set, so the desktop literal-tag
test failed — restored verbatim from the source snapshot. The retained #13
snippet pipeline fixtures answered Ollama without done/done_reason, which the
completed-answer contract rejects; the fake now returns a completed reply, so
all merged snippet assertions pass unmodified.

Verified on this head: locked --all-targets check passes; lib suite passes
299/0 (5 ignored); fmt clean. Strict clippy -D warnings still fails on
pre-existing layering dead code (RetentionMaintenance/CopyTask/selection
probes); not suppressed — downstream layers consume those interfaces.

#11 reconciliation: this head preserves #11's reviewed base invariants
(live-process socket evidence via start_ticks+listening_on, owned-only drop
with no PID-metadata signalling, port joined into the server key, foreign
ports error instead of reuse/adopt, single-server replacement). #11's newer
Stop-authority refinements (LAST_STOP_GEN/STOPPED_FP, suppression expiry)
remain #11-owned; coordinated merge required, no overwrite here.

Adopted #17 8eabfaf contract hardening: provider_contract.rs taken verbatim
(gemini_request/ollama_request builders, tool/refusal-hardened parsers);
llm_cleanup now builds through them with operation plumbing retained.
Adopted parent c261f96 token correction (selection/clipboard verbatim);
owned deliver() tail kept (privacy-epoch persist + Guard outcome).

Reset correction (blocker): foundation keeps the honest worker; Recovery
retains/drains resume jobs; service withholds admission while pending and
drives interactive resets via a `reset` server command (GUI contract:
set_server_cmd, no parallel worker). New regressions: cross-process
second-startup barrier, pending-on-FS-failure, single-flight interactive
completion. #18 competing worker/boot-shortcut BLOCKED, not integrated.

Strict clippy is clean (0 errors): GUI-consumed impls relocated to #23
with restoration payload in docs/audit-handoffs/gui-relocation-23/
(CopyTask surface, baseline adopt, stt wrappers, RetentionMaintenance,
trust_local+tests, async-copy shutdown assertion). reset::start stays as
the service-driven entry. Launch machinery verified byte-identical to
snapshot with real spawn_supervised consumer.

New IO gates: stalled-body cancel abandonment + 20x cancel stress,
deadline-bound test servers (no wedged joins under load).

Remaining: hundreds of cancellation/replacement cycles, panic/recovery,
stalled/partial IO, late-only tail and rolling-window agreement, active
warmup/config changes, authenticated trusted WSS/cloud contracts, final
packaged inference/shutdown/privacy. Historical CUDA successes cover only
tested combinations; Needle/Nemotron accuracy failures stay evidence.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
