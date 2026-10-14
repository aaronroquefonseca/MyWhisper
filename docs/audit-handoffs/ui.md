# ui: independent draft

Source snapshot `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; base main `092cd80626b71396c71ef5f275e34495d75301ad`. Standalone scoped extraction. Locked all-target cargo check passes. Strict lint, Rust/native regressions and live acceptance must be completed on this exact branch; historical integration evidence is not a current passing gate. Preserve MIT/version/iteration workflows and merged protected snippet behavior. Do not merge, tag, release or deploy.

Current scope (R46 GTK focus + overlay only): `src/focus.rs` retains the
previous focus widget through reentrant `focus-widget` dispatch and releases
retired owners outside active borrows plus all owners on window destroy;
`src/app.rs:manage_window_focus` installs it per window; `src/overlay.rs`
checks the GDK backend before the Wayland-only layer-shell probe.
Selectable labels stay enabled. Dictionary/page weak-capture work stays
coordinated with #23; this draft does not duplicate or rewrite it.

Evidence on this worktree (disposable X11, `G_DEBUG=fatal-criticals`):
locked `cargo check --locked --all-targets` pass; strict
`cargo clippy --locked --all-targets -- -D warnings` pass;
`cargo fmt --all -- --check` pass; `cargo test --locked` 109 passed,
0 failed, 5 original opt-in ignores unchanged; extended
`examples/focus_smoke.rs` passes light + dark (32 reentrant transitions,
20 populated rows/theme, stack navigation, En/Es rebuilds, X11 fallback
overlay with `is_layer_shell()==false`, selectable preserved, retired
widgets/windows verified released via weak upgrades); full
`scripts/ui-smoke.sh` passes including the 36-step `ui_preview` smoke in
light/en + dark/es and the logo/toast `desktop_smoke` in both themes.
Remaining: longer repeated edit/navigation/language memory validation and
final native reruns after all implementation changes; full
Rust/native/provider/physical/security/migration/privacy/resource/artifact
acceptance still open. PR stays draft.
