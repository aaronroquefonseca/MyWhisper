# fix(capture): own sessions, provider IO and engine lifetimes

Draft scope: R08, R09, R10, R11, R13, R15, R17, R18, R28, R29, R30, R31, R34, R35, R36, R39, R40, R50.
Source snapshot: `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`; current main baseline: `092cd80626b71396c71ef5f275e34495d75301ad`.
Parent: `fix/audit-clipboard-ownership`.

Largest remaining scope; live WSS/cloud/stress and packaged inference pending. This is an extraction, not a completed or independently certified audit fix. Preserve all regressions and functionality. Shared API integration and current validation are reviewed before ready-for-review. No merge/tag/release/deployment.

Original report, full matrix, source map and historical evidence are owned by the audit tracking PR. The final source snapshot preserves every pre-split correction. Intermediate layers may expose interfaces completed downstream; document every actual compiler/test failure rather than hiding it.
