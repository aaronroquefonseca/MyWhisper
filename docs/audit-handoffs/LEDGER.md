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
| #19 | fix/audit-input-ownership (#18) | d4a82a8 | HOSTED GREEN (run 38612411803): manifest 225 + 4 harnesses, batches cover all 225 exactly once (5 ignored). Nested-stdout fixture correction landed after run 38608553414 diagnosis |
| #20 | fix/audit-verified-installers (#19) | 916a45c | hosted green/strict-zero reported; stacked on input-ownership |
| #21 | fix/audit-clipboard-ownership (#20) | ed08181 | hosted green/strict-zero reported |
| #22 | fix/audit-session-lifecycle (#21) | db1f4e5 | integrating new parent; Operation/admit/CopyTask preserved by source evidence (operation.rs, service.rs, clipboard.rs); shutdown-checkpoint evidence still owed |
| #23 | fix/audit-settings-lifecycle (#22) | 6d6da5e3 | INTEGRATED HEAD, hosted SUCCESS (run 38638287770): 340 + 5 ignored exactly once (345 + 9 harnesses); parent db1f4e5; source-specific acceptance in INTEGRATED-ACCEPTANCE.md (candidate clean, Arch+Nix+UI+audit green on exact SHA); live gaps + PR11 gate still open |
| #11 | fix/server-status-and-switching (main) | bcaff78 | open, ready for review; 10 probes pass reported, 2-phase settlement bug in progress; overlaps #22, preserve on integrate |
| #4 | t3code/be770be5 (main) | 4c7f3873 | open; separate Qwen feature, no competing files |

Stack lag shifts as owners advance (earlier snapshot: #21<#20 by 11,
#22<#21 by 2, #23<#22 by 2). Lag alone is not readiness;
source-specific gates decide. Restack/merge order stays
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

Implementation (corrected commit, #15-owned scripts/): explicit
test-only MYWHISPER_AUDIT_TMPFS_TMP=1 mounts private tmpfs on
root/tmp with kernel-enforced 2 GiB cap (--size 2147483648) and mode
0700 (--perms), owned by the mounting user; a 1 MiB probe mount on
root/.limitprobe proves kernel size enforcement. Prior comments
claiming a budget/0700 without code controls were wrong and are
corrected here: the bound and mode are now mount-level, child-visible
(statvfs totals 2.0 GiB / 1.0 MiB asserted in regressions) and
fail-closed (unknown bwrap options abort the gate).
Preserved: real SQLCipher files/locks/transactions/fsync, SIGKILL/
reopen semantics. No synchronous=OFF, no mocks. No in-scope test
relies on host-reboot persistence (SIGKILL refs are process-reaping
semantics); if a future power-loss test appears it must opt out
explicitly with evidence. Filesystem/power-loss/package/upgrade
gates keep physical/VM filesystems — flag forbidden there.

Evidence: 5 new runner regressions (overlay+cap+mode, default-
no-overlay, invalid-rejected, host-invisibility of tmpfs+probe
contents, 1 MiB ENOSPC at ~1.1 MB without gigabyte allocs) —
test-disposable.py 10/10 pass. Full rusttests fixture under flag in
exact wired form: exit 0, 109/0/5, 3 consecutive runs wall
1.44/1.24/1.24s (runner deterministic on this scope; scope suite is
~1s, unlike #18's). In-namespace peak TMPDIR usage 658 KB vs the
enforced 2 GiB cap. Wiring commit 69a4eea scopes the flag to the
release.yml rusttests invocation only (verified in wired form);
native/package/Nix/physical gates excluded. #18 adopts the corrected
runner commit + 69a4eea exactly and runs hosted suite; #19
(bfd7ce0, still draft, stacked on storage-sync) waits for #18 green
then adopts sequentially.
- main: `Store::checkpoint()` defined, unwired in production.
- #18 tip: called in legacy migration path + tests.
- #22 tip: wired in `shutdown_app` with failure reporting.
- Asks posted on #18/#22 for gate evidence; wiring recorded as
  present-but-ungated until owner evidence lands.

## Hosted batch partitioning (supervisor decision, #15 implements)

No timeout/worker increase. Hosted Rust tests run as 4 explicit
sequential batches sharing checked-in scripts/test-manifest.txt
(4-way stride over sorted lib tests). Each batch gets its own 120s
disposable+tmpfs invocation with unchanged libtest concurrency,
production deadlines and assertions; global 45min job bound and no
retries-to-green unchanged. Whole suite stays REQUIRED locally for
cross-case interference. libtest --exact full paths (no substring
leak); empty bin/example harnesses run whole via --all-targets.

Fail-closed coverage contract: manifest drift vs compiled listing
(missing/extra test/target/ignored change), batch union/disjointness
from logs (exactly once, no unrecognized, identical 5-ignored set),
any batch exit failure or timeout. Target identity uses structured
cargo artifact JSON (never colorized human banners — hosted
CARGO_TERM_COLOR=always broke banner parsing: harnesses parsed as
empty while tests matched); tests/ignored come from each harness
executable's canonical libtest --list; batch logs are ANSI-stripped
before verification and harnesses matched by target-name prefix.
No test removal, payload shrink, output suppression or
producer-count change.

Evidence on this scope: manifest 114 tests + 4 empty harnesses,
verify-manifest current; 4 local batches exit 0 (27+2, 28+1, 28+0,
26+2 = 109 run + 5 ignored); verify-batches proves exactly-once.
test-partition.py 9/9 synthetic + artifact/ANSI regressions
(stride, rejection, malformed/duplicate, 6 fail-closed log mutations,
drift incl. ignored change, artifact selection incl. rlib/build-script
skips + dupes, colored-log acceptance, nonempty-harness refusal).
CARGO_TERM_COLOR=always forced locally regenerates a byte-identical
manifest — the hosted failure mode fixed by construction.

Hosted proof (run 38595511726, head 6838b9a, checks SUCCESS):
`manifest current: 114 tests, 4 empty harnesses` then `batches
cover all 114 tests exactly once (5 ignored), 4 harnesses ran` —
drift gate, all 4 tmpfs batches and the verifier green in CI,
plus every downstream gate in the job. Color-robustness fix
confirmed by hosted pass, not just local forcing.

#18 owes: precise testcase/target inventory (their 195 + ignored
manifest on their branch) + validation of original scope and joined
scope under this mechanism; report concrete constraint if coverage
cannot be preserved faithfully. #19 stays pinned until then.

## R51 packaging analysis (#23: 7011742 bypass → 5aabc5a correction)

Failure (run 38633516808, 343 Rust tests exactly once, then upgrade
gate): `assert pkgver + digest present and SKIP absent` caught
commit 7011742's multi-arch template — sha256sums_aarch64=('SKIP'),
host-only checksum replacement, x86_64-only release fill, dual-system
flake. Exact R51 checksum bypass; gate worked as designed.

Correction 5aabc5a independently inspected (read-only + gates
executed in a disposable /tmp worktree, no sibling touched):
PKGBUILD x86_64-only with plain verified sha256sums; update.sh exit 2
with clear error on non-x86_64 BEFORE any network (proven with shimed
uname -m=aarch64); release.py fills exactly one checksum field;
validate-artifact.py accepts only the x86_64 root (aarch64 probe
archive rejected: member escapes release directory); flake
single-system x86_64 with unchanged check names (desktop,
whisper-vulkan-loader, symbolic-logo — README stays accurate).
SKIP token survives ONLY in the test assertion guard. Upgrade 5/5 and
release 11/11 reproduced on corrected source. MIT/v0.9.4 preserved.
Flake check renames (package-smoke/runtime-loader-test) predate the
correction and stay consistent. Candidate build + hosted Nix/desktop
gates on final source: PREPARED, HELD until #23 hosted stabilizes —
not claimed. No arch expansion without validated artifacts.

## Nested-harness diagnosis (#19 run 38608553414, tip 27b82d6)

Manifest current (223 + 4 harnesses); all 4 batch commands exited 0;
verifier rejected batch-1: one test twice in the same log, one never
executed. Batch logs were not uploaded, so diagnosis is from source
(read-only) plus verifier reproduction — not assumed success.

Root cause (source evidence): #19's
hotkey::tests::saved_startup_mute... re-executes the test binary as a
child (`--exact <itself> --nocapture`) via OwnedChild::spawn, which
uses plain spawn with INHERITED stdout. The child's own
`test ... ok` line lands in the parent harness output (phantom
duplicate) and races sibling result lines without a shared lock, so
the database_key result line is lost/corrupted (never executed).
Real double execution, not a parser artifact: the verifier was right
to fail closed. No #19 code touched.

Mechanism correction (#15-owned): verifier now detects a second
`running N tests` line inside one binary section and fails with an
explicit nested-harness message (reproduced exactly on real output:
same-log duplicate + nested message). Coverage/ignored assertions
NOT weakened. Workflow gains fail-path artifact upload
(batch-*.log + manifest, 7-day retention) so future failures preserve
actual logs — the batch-1.log here is unrecoverable (0 artifacts).

#19 owns the fixture correction (pipe/capture or redirect the nested
child's stdout; do not inherit it) plus its inventory, then re-runs
hosted. #20 gets informed only after an actual #19 head passes
hosted subsequent jobs — pending, not yet sent.

## 120s fixture failure analysis (#18 run 38586390971, tip 1c121c9)

Flag WAS active in CI (bwrap cmdline shows the tmpfs overlay), so
backing is not the whole story. Counts: 200 expected, 194 ok +
5 ignored, 0 failed, exactly 1 unfinished (set-diff vs green run
38584899972): store::segments::tests::
reset_revocation_and_untrust_remove_both_directions_and_deny_old_work.
Final completions flowed until 5s before the kill — budget exhaustion,
not a silent deadlock.

Unfinished-test inspection (read-only, 1c121c9 src/sync_segments.rs):
pure synchronous SQLite ops (stores/apply/sync/reset/export), no
sockets, children, channels, sleeps or watchdogs — CPU-bound tail
victim, and also-last in the green run. 1c121c9 is docs-only vs
b20df2b (storage.md 1 line), so behavior is identical; outcome
variance is environmental.

Slow-test wall deltas, green vs failed (same code+fixture):
oversized_record 10s vs 66s+ (60s warning fired, then passed);
large_encrypted_history ~25s vs ~44s; corrupt_final_part ~10s vs
~22s; idle_and_backward ~0s vs ~20s. Suite totals 75s green vs
120s-killed. Pattern: up to 6x per-test CPU variance on shared CI
runners compounds across many heavy encrypted-loopback tests past
the fixed 120s budget. No OOM/allocation failure in the log. Reaper
behaved: TimeoutExpired at 120s, killpg SIGTERM→SIGKILL path, exit 1,
no suppression. b20 green is genuine but NOT a stable gate. No
timeout/worker/assertion changes made. #19 stays pinned (bfd7ce0,
draft).

## Publication hold

Held. No merge, tag, release or deploy from this branch.
