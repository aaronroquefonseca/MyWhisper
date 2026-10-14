# R01–R51 ownership/dependency ledger (PR #15 coordination)

Stable tested parent for this branch: `main` at `092cd80`
(MIT, v0.9.4, literal snippet semantics). This branch adds docs
only; `src/` identical to parent. Scope readiness (owner's gates on
their tip) is distinct from final integrated release acceptance
(#15, on the integrated tip, all gates green).

## Layer status, 2026-10-14 (fresh fetch)

| PR | Branch (base) | Tip | Scope gate state |
| --- | --- | --- | --- |
| #15 | docs/audit-work-handoffs (main) | (this push) | Shared security commit c1c42b0 (R51, supervisor-authorized): rustls 0.23.45→0.23.46 for RUSTSEC-2026-0346–0349, adapted to main lock graph (tempfile→getrandom 0.3.4 per manifest; none of #18's storage-scope deps). Audit 0 vulns/warnings; fmt, strict Clippy (rustls .46 rebuilt from source, crate re-linted), 109 tests, release 11, upgrade 5 green. #16/#17 reuse SAME commit via cherry-pick; later rebases after owner merge drop it naturally. Hosted run 38573390119 cause recorded: steps 1–9 passed; step 10 never ran cargo audit — cargo install exited 1 on cached exact binary under sh -e (workflow idempotency fix retained, no owner conflict) |
| #16 | fix/audit-ui-ownership (main) | da4f08e | GREEN reported (strict Clippy + checks) |
| #17 | fix/audit-provider-contracts (main) | 8eabfaf | GREEN reported (strict Clippy + checks) |
| #18 | fix/audit-storage-sync (main) | 8733bfe→d09ce1d | d09ce1d: reset-barrier correction (competing GUI worker removed, relocation contract to #22/#23) + rustls 0.23.45→0.23.46 for RUSTSEC-2026-0346..0349, audit clean reported (IDs + patch verified identical to independent check). Still BLOCKED/unresolved: hosted all-target 120s timeout fails; gate evidence still owed. Owns the dependency correction; #15 reverted its duplicate bump |
| #19 | fix/audit-input-ownership (#18) | c7479bc | strict lint FAIL from layering |
| #20 | fix/audit-verified-installers (#19) | 169a1cf | strict lint FAIL from layering |
| #21 | fix/audit-clipboard-ownership (#20) | c261f96 | strict lint FAIL from layering; 11 commits behind #20 tip |
| #22 | fix/audit-session-lifecycle (#21) | a3ccf74 | new head, no hosted checks yet; owned-shutdown checkpoint wired, evidence asked; 2 behind #21 |
| #23 | fix/audit-settings-lifecycle (#22) | feb531e | CI failed: oversized throttled encrypted sync + 120s runner limit; 2 behind #22 |
| #11 | fix/server-status-and-switching (main) | 7c066b16 | open, ready for review; overlaps #22, preserve on integrate |
| #4 | t3code/be770be5 (main) | 4c7f3873 | open; separate Qwen feature, no competing files |

Stack lag verified by merge-base: #21 behind #20 by 11,
#22 behind #21 by 2, #23 behind #22 by 2. Lag alone is not
readiness; source-specific gates decide. Restack/merge order stays
with the owning agents; #15 does not restack or force-push them.

## API changes by #15

None in code APIs. Workflow change (R51, main-based, no sibling
owns it): audit step installs cargo-audit 0.22.2 only when the exact
binary is absent; `cargo audit --deny warnings` still always runs.
Dependency correction (rustls 0.23.46) shared: #18 d09ce1d owns
the chain copy; #15 commit c1c42b0 is the supervisor-authorized
main-based twin (identical IDs/patch, no storage-scope deps) so
#15/#16/#17 root CI gates pass. #16/#17 reuse it via cherry-pick;
later rebases after owner merge drop the duplicate naturally.

## Tmpfs scratch investigation (supervisor decision, #15 implements)

Mismatch confirmed, not assumed: disposable.py creates the gate root
on the host /tmp, then bwrap --tmpfs /tmp hides it but --bind root
re-exposes host-backed storage as TMPDIR. Measured fsync: tmpfs
0.002ms vs btrfs 8.7ms (~4000x). Local /tmp is tmpfs; CI container
/tmp is overlay disk — same tests, radically different fsync bills.
No speculative-counter redesign; no sharding; 120s budget unchanged.

Implementation (one commit, #15-owned scripts/): explicit test-only
MYWHISPER_AUDIT_TMPFS_TMP=1 overlays private tmpfs on root/tmp only
(bwrap --tmpfs after the root bind; in-namespace UID stays euid,
owner-writable; host-side 0700 stub + marker/HOME/XDG unchanged; var
popped so child never inherits; any other value fails closed).
Preserved: real SQLCipher files/locks/transactions/fsync, SIGKILL/
reopen semantics. No synchronous=OFF, no mocks. No in-scope test
relies on host-reboot persistence (SIGKILL refs are process-reaping
semantics); if a future power-loss test appears it must opt out
explicitly with evidence. Filesystem/power-loss/package/upgrade
gates keep physical/VM filesystems — flag forbidden there.

Evidence: 4 new runner regressions (overlay+ownership+roundtrip,
default-no-overlay, invalid-rejected, host-invisibility of tmpfs
contents) — test-disposable.py 9/9 pass. Full rusttests fixture
under flag: exit 0, 109/0/5. In-namespace peak TMPDIR usage 658 KB
vs 2 GiB investigation budget. #18 owes: their-scope peak
measurement (oversized sync suite) + full fixture validation under
the flag; sharding stays an evidence-backed fallback only.
- main: `Store::checkpoint()` defined, unwired in production.
- #18 tip: called in legacy migration path + tests.
- #22 tip: wired in `shutdown_app` with failure reporting.
- Asks posted on #18/#22 for gate evidence; wiring recorded as
  present-but-ungated until owner evidence lands.

## Publication hold

Held. No merge, tag, release or deploy from this branch.
