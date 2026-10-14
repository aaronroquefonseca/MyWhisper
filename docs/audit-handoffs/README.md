# Remaining audit work: draft ownership and acceptance

This handoff supersedes the *current-status claims* in the preserved historical
status/matrix. The historical probes assert buggy behavior and are evidence,
not a future passing suite.

Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186` (includes the formerly uncommitted
empty-provider-list migration and its regressions). Current extraction base:
`092cd80626b71396c71ef5f275e34495d75301ad`. The original implementation worktree and immutable audit
branch `f018ab855af41759e275c80708d67973f2890587` remain unchanged.

PRs #5/#6/#7 are merged. Main now includes MIT licensing/version 0.9.4, iteration
workflows and the protected literal snippet behavior merged in #13. Preserve
those changes. Older evidence referring to no owner license/open PRs/0.9.3 is
historical, not a current instruction to undo them. No automatic merge, tag,
release or deployment is authorized by this handoff.

Three drafts target main independently. The state/capture core uses a focused
stack: storage/sync → input → verified installers → clipboard → sessions →
settings/native integration. Existing ownership, reset, asset-lease, policy-epoch
and completion contracts create cycles across these scopes. This split preserves
those contracts and all source/tests; intermediate drafts must not be merged
until their actual interface/validation requirements are satisfied. Each agent
owns one scope and must coordinate changes to shared interfaces with its parent.

The full original matrix, report, before/after manifests, logs, scripts and
historical reproductions are retained alongside this handoff. They establish
what was executed on historical source, not that a newly extracted draft passes.
Draft-specific checks and remaining acceptance must be recorded separately.

Paid-cloud disposable credentials and successful trusted authenticated WSS were
not established. Physical device changes, cross-host LAN pairing/revocation,
compositor variants, running upgrade/recovery, migration fault/backup/privacy
review and final packaged inference/stress remain genuine acceptance gates.
Continue independent work and report exact missing prerequisites; fixtures do
not replace passing live integrations. Use private disposable data exclusively.

The completed findings ownership is in FINDINGS.md; source-inventory.json assigns all 459 original changed paths. PR-15-PROMPT.txt through PR-23-PROMPT.txt provide independent copyable handoffs. Extraction compilation and limited strict-lint logs are retained separately; these are not current full-suite/live passes.
