# fix(input): isolate microphone and hotkey ownership

Draft scope: R14, R23, R24, R26, R27, R33.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-storage-sync`, pinned stable SHA `3ea96b4` (code-identical to hosted-green `8603be1` plus green-record doc; integrated after FULL hosted success `38589669408`).

Depends on owned runtime/storage APIs; physical/layout acceptance pending. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Evidence (head `48a03a76e8e7ea5849f6489fc78e22fff8355f47`, parent `3ea96b4`/code `8603be1`): `cargo fmt --check` clean; `cargo clippy --locked --all-targets -- -D warnings` clean (0 warnings); `cargo test --locked --all-targets` 218 passed / 0 failed / 5 ignored locally; hosted run `38591111007` SUCCESS with suite 92.74/120 s (oversized case >60 s warning — marginal, nondeterminism tracked by #18, not claimed stable); `scripts/cpal-smoke.sh` 9/9 (F32/I16/U16 x 1/2/6); native uinput Down/Up/FD gate passes. R23 saved-mute now applies via `apply_input_preferences` before listener startup; R24 service configures capture suspension (`configure_capture_suspend`) while engine ack stays with #22 and frontend request with #23. Parent-gated `child::run`/`read_bounded` are replaced in production by `run_guarded`/`read_private`; test-only uses unchanged.

Remaining live gates: physical device changes, non-US layouts, real service capture suspension (never against the live user service; controlled disposable scope only); longer slow-inference memory/latency measurement. Local native uinput gate passes (`MYWHISPER_NATIVE_HOTKEY_GATE=1 virtual_keyboard_fires_down_up`: own virtual keyboard, exact Down/Up edges, FD cleanup). PR stays draft.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
