# fix(input): isolate microphone and hotkey ownership

Draft scope: R14, R23, R24, R26, R27, R33.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-storage-sync`.

Depends on owned runtime/storage APIs; physical/layout acceptance pending. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Evidence (rebased onto `fix/audit-storage-sync@0d1fa4b`): `cargo check --locked --all-targets` passes; `cargo clippy --locked --all-targets` has no mic/hotkey/audio warnings; 49 mic/hotkey/audio/settings lib tests pass; `scripts/cpal-smoke.sh` passes F32/I16/U16 x 1/2/6 channels; `net::tests::oversized_record_survives_throttled_encrypted_rounds_and_lost_ack` passes with parent hardening. R23 saved-mute now applies via `apply_input_preferences` before listener startup; R24 service configures capture suspension (`configure_capture_suspend`) while engine ack stays with #22 and frontend request with #23. Parent-gated `child::run`/`read_bounded` are replaced in production by `run_guarded`/`read_private`; test-only uses unchanged.

Remaining live gates: physical device changes, non-US layouts, real service capture suspension; longer slow-inference memory/latency measurement. PR stays draft.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
