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
disposable nested niri only): delayed consumption, queued
cancellation/reset, empty/text/binary/multiple-MIME offers, user replacement,
copy-only/no-op, fallback failure and supported compositor/paste variants.
The example now includes a multiple-MIME roundtrip; an earlier private niri
run does not certify every compositor, paste backend/combo, or this head.
Locked `--locked --all-targets` check and the clipboard/selection/child/
operation/runtime unit subset pass on this head; strict lint, full regression
and live gates remain pending. Capture-edge authorization/completion is
coordinated with #22 (which replaces the legacy `transcribe::deliver`
adapter) and user feedback with #23.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
