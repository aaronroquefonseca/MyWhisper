# PR11 clearance mapping → #22 session/server source

Reviewed: PR11 `c4c64c5940e845e039ee9c2b24417a8fbfe3ec5a` (v15 cleared) against #22
head `2388c80`. Method: no blind cherry-pick (different base); per-probe
equivalence analysis against actual #22 mutation admissions. Verdict: the
portable core (generation+intent validation at every admission,
instance-scoped settlement/cleanup, live-socket evidence, accepted-plan
domain separation) is already present with adapted regressions. The
Stop/suppression/expiry/reconcile machinery is absent by accepted design
(zero `STOPPED_FP`/`LAST_STOP_GEN`/`reconcile_servers`/`ReconcileAuthority`
symbols in `src/`); those probes are N/A with evidence, not silently
omitted. No production change resulted; no GUI contract change; #23
untouched.

## Applicable probes — covered with adapted regressions

- P3 torn settings/generation snapshot discarded. #22 admits ONE bundle:
  press builds `sess.{plan,backend,sherpa_backend,net}` together and
  `ensure_owned` consumes exactly that bundle (`transcribe.rs:3911`;
  warm path carries `EngineConfig{plan,backend,sherpa_backend,net}` +
  gated operation, `:4430`). Nothing re-reads settings mid-admission.
  Regression: `delayed_release_cannot_detach_or_capture_samples_for_the_next_recording`
  (admitted session coherence across release).
- P5/P10 superseded lazy/save-only restore retains original server. #22
  model REPLACES on key change by accepted design (single-server rule):
  `ensure_server` key+port+assets compare under `SERVER_START_LOCK`
  (`:736-751`), explicit "key changed; replacing" path. Regressions:
  `foreign_server_markers_and_stale_pid_metadata_never_grant_ownership`,
  `local_readiness_probe_cancel_reset_and_stop_reap_owned_servers`.
- P8 superseded ensure admission retains ORIGINAL PID/socket/map.
  Same accepted-model note as P5/P10: replacement (never adoption of
  foreign state) + foreign-port refusal `:752-756`. Same regressions.
- P4/P9 validated observation vs restored config. Live-socket evidence
  only: `server_status_while` requires recorded start time AND socket
  ownership (`:543-562`); local map authoritative when present; PID
  metadata never signals (`reap_stale_servers` discards metadata,
  `:508`). Regressions: `process_status` suite + readiness tests.
- P6/P13 Stop-wins / no resurrection / coherent settlement. #22 Stop
  equivalent is `drop_servers`: generation bump FIRST (`:568`), then
  queue/map/pidfile mutation. Every ensure-bound operation carries a
  `server_generation() == sampled` gate (press `:4280`, warm `:4415`,
  `ensure_for_plan_at_generation` `:4101`, tests `:6914`), revalidated
  by `reset_check` (= `operation.check`) at admission entry, pre/post
  spawn, every wait iteration, post probe (`:720,757,759,783,807`).
  Abort paths remove+kill only their own instance (`StartupGuard::drop`
  `:832`, `EngineUse::drop` `:4147`, readiness-owner checks `:789,808`).
  Regression: `local_readiness_probe_cancel_reset_and_stop_reap_owned_servers`
  (cancel/reset/`drop_servers` mid-stalled-readiness → error <500 ms,
  map empty, child reaped, lease released).
- P11/P12 revoked intent defers; fresh polls start clean. Engine-lease
  + generation gates: stale warm/press aborts, fresh admission proceeds
  (`ensure_for_plan_at_generation`, `WARM_QUEUE` coalescing without
  mid-inference replacement `:4366`). Regressions:
  `engine_ownership_wait_and_startup_cancel_without_affecting_another_owner`,
  `configuration_warmup_cannot_replace_active_inference`,
  R08 300-cycle stale-session suite.
- Accepted-session/plan distinct domain (review §59). #22 has no
  watcher/settings-identity to follow: `ensure_for_plan` takes an
  explicit caller plan (accepted sessions pass their own), warmups are
  owner/generation-bound, no blanket restriction exists. Correct by
  construction; regressions: press/deliver suites.

## N/A with evidence (feature absent, not omitted)

- P1/P2 Stop/expiry orderings, P6-suppression, P7 bookkeeping, P9-outer
  fingerprint, P11/P12/P13 suppression/expiry/settlement specifics:
  no Stop/suppression/expiry/reconcile/settlement machinery exists in
  this tree (symbol absence verified). The control intents they govern
  (GUI stop, reset, provider switch) are covered above through
  generation gates + instance scoping + the readiness test.
- Settings-only saves under admission: no background settings applier
  exists (no watcher/reconcile thread); settings apply on next
  admission read. Service `stop`/`start` commands are destructive by
  design (immediate, like cleared explicit Stop) and serialize through
  the service loop.

## Preserved invariants (unchanged by this analysis)

Input-edge ownership, bounded admission, cancellable IO, leased
engines/assets, ordered finalization with handle joins, token-correct
snippets, completed-answer contracts, honest reset worker + barriers,
service-driven interactive reset, async copy leg, provider rendezvous
fixtures. Strict clippy zero, 335-test inventory green (330 run /
5 ignored), hosted CI green on this head.
