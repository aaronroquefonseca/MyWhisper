# fix(clipboard): own delivery and preserve selection privacy

Draft scope: R32, R45, R39 clipboard boundary.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-verified-installers` at `5edeeab8fd3167a93dd5813f0099c35b087677b9`
(code `1344bd433b58be484c005b0723da8fdc25d3035b`, strict-zero; eight clipboard
commits rebased onto it).

Relocation executed: the parent keeps only the real
`selection::Guard` type (byte-identical, consumed by
`crate::operation::Operation::guard`); the full owned-selection
implementation lives in this layer. Wayland client deps stay on the shared
#18 foundation. `CopyTask`/`copy_text_async` are reserved for #22's
capture-edge wiring (unused here by construction, neither stubbed nor
deleted); `Operation`'s wider contract stays shared with #22.

Restack onto `367f54a` (merge `04cce9f`): one content conflict
(`src/selection.rs` vs the parent relocation stub) resolved to this layer's
implementation body with the frozen `Guard` verified byte-identical; the
parent's `transcribe.rs` input/retention rework (atomic `capture_settings`,
idle-capable `maintain_retention_at`) merges cleanly around the preserved
same-store-epoch deliver tail. The parent moved production `child::run`
behind `#[cfg(test)]`, so the ydotool key-release cleanup now uses
`child::run_guarded` with the live delivery guard (no cleanup fork after
cancellation). Reconciled gates: check/fmt clean, full suite 270 passed /
0 failed, live nest wtype CtrlV/CtrlShiftV/ShiftInsert green, ydotool still
fails closed (`exit status: 2`, no hang). #18's partition runner + manifest
(`scripts/partition-*.py`, `scripts/test-manifest.txt`) touch no scope file
and are not required by these gates; nothing carried, nothing replaced.

Actual compositor variants and final privacy review pending. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Continuation (this head): bounded owned selection actor + FIFO finalization
serializes delivery (R32); copy/arm/consumed verify actual completion and
selection ownership instead of reporting spawn success, and restoration is
conditional on still owning the selection (R45); borrowed transcripts always
carry the sensitive hint and the previous selection is restored only when
still owned (R39 delivery boundary). Snapshot now carries its offer identity
and the copy verifies it first, so a user replacement between snapshot and
copy aborts instead of overwriting fresh user data with stale content.

Protocol limits (documented in `src/selection.rs` / `src/clipboard.rs`):
`set_selection` has no atomic ownership-conditional restore —
`restore_if_owned` is sync-check-then-set and a replacement inside that
roundtrip window is overwritten. Reads are anonymous, so an eager clipboard
manager counts as `consumed` completion and can trigger early restoration
before the focused target reads; borrowed payloads stay sensitive-by-default
to ask compliant managers to skip them, but managers that ignore the hint are
not distinguished. `wl-copy --foreground` fallback covers copy-only/Keep-on
delivery where data-control is missing and fails closed otherwise.

Live gate (`scripts/clipboard-smoke.sh` + `examples/clipboard_smoke.rs`,
disposable nested niri only, synthetic data): all three supported wtype
combos pass on this head — CtrlV, CtrlShiftV and ShiftInsert
(`MYWHISPER_CLIPBOARD_PASTE_COMBO` override, CtrlV default; the synthetic
target accepts Ctrl+V/Ctrl+Shift+V/Shift+Insert). Each run covers delayed
consumption (650 ms floor vs 700 ms delayed target read),
empty/text/binary/multiple-MIME restoration, user replacement aborts without
touching fresh content, mid-pipeline cancellation restores the predecessor,
copy-only keeps the transcript, no-op leaves the selection untouched. The
example also adds a `MYWHISPER_CLIPBOARD_PASTE_BACKEND=ydotool` override for
manual runs on real sessions.
A ydotool attempt in the disposable nest fails closed at paste
(`exit status: 2`, daemon socket isolated by the disposable XDG_RUNTIME_DIR)
and the pipeline reports the error instead of success; the socket must not be
bridged into the nest (uinput keys would leak to the host session), so live
ydotool injection is a valid isolation result but not an integration pass.
Still pending and not certified: compositors without data-control (fallback
path; only niri is harnessed, mutter is present but unharnessed) and
manager-heavy sessions (anonymous-reader limitation, documented above).
Locked `--locked --all-targets` check, `cargo fmt --check`, and the full
unpartitioned `cargo test --locked` suite pass on every head (latest
reconciled: 272 passed, 0 failed; 5 ignored are pre-existing live-model
tests). `cargo clippy -- -D warnings` fails only on the seven bound symbols
(CopyTask trio → #22 async copy; `admit` + `operation` items → #22;
`trust_local` + `model_present` → #23): tracked shared fate, no suppression
or deletion. Capture-edge authorization/completion is coordinated with #22
(which replaces the legacy `transcribe::deliver` adapter) and user feedback
with #23. Parent #20 must satisfy its own scope before any merge.

Ownership-v2 gates on the reconciled head: strict `clippy -D warnings`
zero diagnostics, fmt clean, own manifest 273 tests (268 run / 5 ignored;
parent 262 + this layer's 6), four hosted-identical disposable batches
exact-once green, full unpartitioned suite 268/0/5 on healthy scratch,
live nest wtype CtrlV/CtrlShiftV/ShiftInsert green, ydotool fails closed.
Guest gate config `packaging/nixos/clipboard-guest.nix` (+ `clipboard-guest`
flake check, app package parameterized for #15's candidate): Sway MIME
round-trip, wtype + guest-ydotoold injection with byte equality,
stuck-modifier sentinels, manager observation, guest-only isolation asserts;
app-writer/fallback/mutter/hint-skip legs explicitly OPEN. Validated offline
only (nix parse with dummy store, embedded-driver py_compile) — #15 is sole
VM/store runner. Box finding OPEN: /tmp at 77% (co-tenant scratch,
not mine to delete) makes full-suite staging writes fail with SQLite
write-class errors in untouched store/sync tests; same tests pass solo on
healthy scratch and in every batch — cause established, not waived, not
called fixed; hosted healthy runner arbitrates.
Restacks: onto `367f54a` (one selection-stub conflict → implementation body,
frozen `Guard` identical; `child::run` gating → guarded cleanup), onto
`aebdbd4` (7/7 zero-conflict), and onto ownership-v2 `5edeeab8`
(restored `src/selection.rs` wholesale over the parent deletion — one
modify/delete conflict — plus `mod selection;`; `operation.rs` deletion
propagates: deliver authorizes through the new owned `DeliveryAuth`
(file-identity + epoch + reset rechecks, sticky denials, historic message
strings, two ported contract tests) instead of the #22-owned `Operation`;
unreachable async-copy scaffolding (`CopyTask`/`poll`/`copy_text_async`,
zero callers) removed rather than silenced — re-add from
`4123680:src/clipboard.rs:245-290` if capture-edge/GUI ever needs it.
`installed_asset` and `net.rs` take the parent side untouched; handshake
fixture `b842a0d` lives down-stack in #18, preserved by not touching it).
Adopted #15 `e2751d2` verbatim
(nested-harness detection, its unit test, batch-log artifact upload).
Regenerated own manifest on the joined tree (277 lib tests: 272 run /
5 ignored; parent's copy lacked 54 artifact tests now listed);
`verify-manifest` current, `test-partition.py` 10/10 OK. Net-handshake Broken
pipe under load stays OPEN (unobserved this round; a solo pass would not close
it). Reconciled live nest: wtype CtrlV/CtrlShiftV/ShiftInsert green on a
fresh binary; ydotool fails closed with no hang.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
