# fix(clipboard): own delivery and preserve selection privacy

Draft scope: R32, R45, R39 clipboard boundary.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-verified-installers`.

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
`cargo test --locked` suite pass on this head (251 passed, 0 failed;
5 ignored are pre-existing live-model tests). `cargo clippy -- -D warnings`
still fails only on pre-existing cross-scope dead code owned downstream
(session/process-launch, store/net APIs completed by #22/#18); nothing in the
touched clipboard/selection/deliver/smoke lines warns, and no suppression or
deletion was added. Capture-edge authorization/completion is coordinated with
#22 (which replaces the legacy `transcribe::deliver` adapter) and user
feedback with #23. Parent #20 must satisfy its own scope before any merge.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
