# PR11 clearance mapping → #22 session/server source

Reviewed: PR11 `c4c64c5940e845e039ee9c2b24417a8fbfe3ec5a` (v15 cleared) and
the integration probe map against #22 head (this branch). Method: bind
each probe to equivalent USER-VISIBLE behavior in this architecture —
engine Stop/autostart/lazy/keep-warm as the GUI advertises them — with
actual public APIs, never bare symbols. Implementation removal is
distinguished from preserved behavior throughout. No blind cherry-pick
(different base); no GUI change; #23 untouched.

## Owned live behavior (what the user sees)

- Engine Stop (GUI Stop button, service `stop` command, reset, provider
  switch away): supervised servers die immediately
  (`transcribe::drop_servers`: generation bump first, then warm-queue,
  map and pidfile mutation). No resurrection: every ensure-bound
  operation carries a `server_generation() == sampled` gate
  (press `transcribe.rs:ensure_owned`, warm `warm_plan`,
  `ensure_for_plan_at_generation`, tests), revalidated by
  `reset_check` at admission entry, pre/post spawn, every wait
  iteration and post probe (`ensure_server`). Aborts remove and reap
  only their own instance (`StartupGuard::drop`, `EngineUse::drop`,
  readiness-owner checks).
- Autostart toggle: service-loop STT reconcile (`service.rs:
  stt_reconcile_tick`, every 2 s poll) compares stored
  `(primary, autostart)` against last applied. Toggle ON warms the
  current configuration now (reuse when current) unless a Stop landed
  after this primary was last observed disabled (per-primary disabled
  marker: newer Stop revokes the toggle until a fresh deliberate
  change); toggle OFF keeps the running server with no bounce.
  Failures retry without advancing. First observation reconciles
  current intent instead of assuming startup applied it.
- Primary (configuration) change: stale supervised servers must not
  serve — retired first, then warmed when autostart is on (lazy state
  recorded when off). Active inference/capture defers any transition;
  reset pending never reaches reconciliation (service barrier).
- Keep-warm: background reaper (`spawn_server_reaper`) re-reads the
  setting live every 30 s; idle-past-timeout servers stop; active
  sessions/finalizations and held engine leases are never reaped.
- Lazy start: first press ensures its admitted plan (no server needed
  beforehand); autostart off never blocks it.
- Status: live listening sockets + recorded start times only
  (`server_status_while`); PID files are metadata, never authority.
- Accepted plans (capture sessions, releases, fallbacks) carry their
  own admitted bundle (plan+backends+net) and operation chain;
  no current-settings restriction applies to them.

## Thirteen probes bound

- P01 old update cannot borrow newer Stop; polls stay cold. Bound to
  generation-gated admission + drop-wins: a stale gated operation
  aborts, nothing starts, status stays cold. The tick revalidates live
  pair + generation immediately before destructive control and holds
  the engine lease across the drop, so admitted inference is never
  destroyed. Regression:
  `local_readiness_probe_cancel_reset_and_stop_reap_owned_servers`
  (cancel/reset/`drop_servers` mid-stalled-readiness → error <500 ms,
  map empty, child reaped, lease released).
- P02/P06 old enable/expiry cannot clear newer Stop; suppression
  preserved, polls cold. No expiry/suppression state exists in this
  architecture (stopped = gone from the map + killed; there is no
  suppressed-but-present state to resurrect). Replacement coverage:
  drop-wins + reset barriers + `WARM_QUEUE` cleared on every Stop
  (`drop_servers`), pinned by the readiness test above and
  `second_startup_never_clears_barrier_of_active_reset_worker`.
- P03 torn settings/generation snapshot discarded. Control snapshot
  (generation witness before settings read, recheck after) discards
  torn observations with retry-next-poll in `stt_reconcile_tick`;
  admission bundles (plan+backends+net) travel immutably from one
  settings load (`ensure_owned` call sites; `EngineConfig`).
  Regressions: `stt_tick_torn_snapshot_defers_without_advancing`
  (rendezvous-parked transition defers, baseline unadvanced) +
  `delayed_release…` (admitted-session coherence).
- P04 same old authority rejected; nothing resurrects; later change
  possible. Stale gated admission errors with map empty (tick
  revalidation aborts on pair/generation drift before any drop; warm
  paths revalidate plan identity at execution); fresh generation
  admission starts normally.
  Regressions: readiness test above + `fresh_admission_reuses_current_and_restarts_after_stop_
  with_cold_polls` (fresh start, new PID, live polls) +
  `warmup_superseded_detects_settings_moves` (admitted-vs-live plan
  identity predicate shared by tick and deferred warm paths).
- P05 superseded lazy preserves running server. Toggle-OFF (same
  primary) performs no mutation by construction; stale warmups die on
  their generation gates. Regressions:
  `stt_tick_disable_keeps_running_server_without_bounce` (PID +
  listener identical across disable) + `configuration_warmup_cannot_
  replace_active_inference`.
- P07 deferral marks nothing applied. No applied-state bookkeeping
  exists (deferral = error + unchanged pair + unchanged map); valid
  suppressed settling has no counterpart because stopping is
  terminal. Replacement coverage: the unresolvable-plan half of
  `stt_tick_toggle_on_warms_current_and_unresolvable_retries`
  (failure advances nothing) + deferred-warmup retirement test.
- P08 superseded autostart rejected at admission; original survives;
  no temp listener. Entry, generation (via operation check), port and
  post-spawn checks gate every admission; stale entry-check failures
  abort pre-mutation, and foreign ports are refused, never adopted
  (`ensure_server`). A same-port replacement removes the stale entry
  before the port check precisely so the freed socket (not a foreign
  listener) is observed — verified by the replacement suites, which
  fail deterministically if this order is disturbed. Regressions:
  fresh test (pre-cancelled attempt leaves PID, listener, singleton
  map untouched) + foreign-marker tests.
- P09 latest authority + live intent before suppression mutation.
  No suppression to mutate; the analog is the dual-authority delivery
  guard (`Operation::guard` AND `DeliveryAuth::permitted`, rechecked
  per use + post-hoc outcome mapping). Regressions: async/sync leg
  mapping tests + DeliveryAuth contract tests.
- P10 leaving-local preserves original. No leave-local/foreign-apply
  path exists: nonlocal observations are refused (`SelfHostedWs`
  never spawned; foreign ports error), never applied. Compatibility
  decision: refusal replaces leave-local destruction. Regressions:
  foreign-port/listener tests + `retire_invalid_assets` suite.
- P02/P06/P09-core same-target Stop stays cold across unchanged
  polls. With settings unchanged the tick pair never moves, so no
  warm path can resurrect a stopped server; Stop intent needs no
  stored suppression because stopped is terminal (empty map, dead
  child, cold status).
  Regression: `stt_stop_stays_cold_across_unchanged_polls` (three
  unchanged polls after Stop: map empty, socket closed, pair
  unchanged). Command-loop coherence:
  `server_stop_command_and_tick_stay_quiet_together` (explicit stop
  command consumed in service order, then tick stays quiet).
- P11/P12 revoked intent defers; fresh restore starts normally; Stop
  retained; no false application. Stale tick observations abort on
  pair/generation revalidation before any drop; stale warm/press
  aborts via generation gates + plan-identity checks; explicit
  `reset` command + drop paths stay immediate; fresh generation
  admission starts (asserted new PID); toggle-ON after a newer Stop
  of the same primary defers via the disabled marker until a fresh
  deliberate change.
  Regressions: fresh test + `engine_ownership_wait_and_startup_cancel_
  without_affecting_another_owner` + 300-cycle stale-session suite.
- P13 one coherent control/target/live-settings snapshot; fresh B
  starts, C stays cold. Single-server invariant + key replacement make
  multi-target settlement unrepresentable: at most one supervised
  server exists; Stop clears it; fresh polls start exactly the current
  primary. Compatibility decision: target-scoping via replacement,
  not suppression bookkeeping. Regressions: fresh test (5 status
  polls across cold/live/drop/fresh/cold + quiescence) +
  `stt_tick_primary_change_lazy_stops_when_auto_off`.

## Contrasting positives retained

Newer-Stop-wins, coherent-restore, fresh-start-without-extra-action,
same/cross-engine replacement baselines, first-cold-start ownership,
press-warmup usefulness, accepted-plan authority, active-use
protection with immediate Stop, cancel/release/takeover/teardown
accounting, empty-Stop, reaper stability, foreign/PID safety,
deduplicated status, identity fields, snippet protection — covered by
the suites named above plus the unchanged 335-test inventory (330
run / 5 ignored), strict clippy zero, hosted CI green.

## Preserved invariants

Input-edge ownership, bounded admission, cancellable IO, leased
engines/assets, ordered finalization with handle joins, token-correct
snippets, completed-answer contracts, honest reset worker + barriers,
service-driven interactive reset, async copy leg, provider rendezvous
fixtures, single-server replacement (never adoption).
