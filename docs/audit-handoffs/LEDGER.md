# R01–R51 ownership/dependency ledger (PR #15 coordination)

Stable tested parent for this branch: `main` at `092cd80`
(MIT, v0.9.4, literal snippet semantics). This branch adds docs
only; `src/` identical to parent. Scope readiness (owner's gates on
their tip) is distinct from final integrated release acceptance
(#15, on the integrated tip, all gates green).

## Layer status, 2026-10-14 (fresh fetch)

| PR | Branch (base) | Tip | Scope gate state |
| --- | --- | --- | --- |
| #15 | docs/audit-work-handoffs (main) | (this push) | GREEN this turn: fmt, strict Clippy, 109 tests, release 11, upgrade 5, CLI 2, audit 0 vulns (rustls 0.23.46), UI smoke both themes, Arch install+upgrade, Nix logo/desktop-VM/loader-VM. Hosted run 38573390119: steps 1–9 passed; step 10 never ran cargo audit — cargo install exited 1 on cached exact binary under sh -e. Fixed by idempotent audit step (same file, below) |
| #16 | fix/audit-ui-ownership (main) | da4f08e | GREEN reported (strict Clippy + checks) |
| #17 | fix/audit-provider-contracts (main) | 8eabfaf | GREEN reported (strict Clippy + checks) |
| #18 | fix/audit-storage-sync (main) | 8733bfe | BLOCKED, unresolved (not stable foundation): inspected GUI worker suppresses filesystem errors; boot blindly clears pending-reset marker; owner accepted relocation/removal of competing path, correction underway; hosted all-target 120s timeout still fails. Checkpoint migration-path use stands, gate evidence still owed |
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
Dependency change: `rustls` 0.23.45 → 0.23.46 in Cargo.lock
(patch bump, no API change) resolving fresh RUSTSEC-2026-0346–0349;
audit green, fmt/strict-Clippy/109-tests green on bumped tree.
Siblings rebasing onto main will inherit both via merge.

## Checkpoint finding (source evidence, read-only)

- main: `Store::checkpoint()` defined, unwired in production.
- #18 tip: called in legacy migration path + tests.
- #22 tip: wired in `shutdown_app` with failure reporting.
- Asks posted on #18/#22 for gate evidence; wiring recorded as
  present-but-ungated until owner evidence lands.

## Publication hold

Held. No merge, tag, release or deploy from this branch.
