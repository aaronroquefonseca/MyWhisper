# Authenticated sync compatibility and progress

The Noise XX channel and signed protocol-1 hello remain compatible with
existing peers. An optional `pages: true` hello capability selects the paged
format only when both peers support it. Peers without it retain the small
full-snapshot format. An oversized legacy export fails with an upgrade message
before constructing the full record vectors; local data remains intact.

Paged peers exchange a manifest containing a random persistent database stream
ID and an upper SQLite sequence, then their committed receive cursors for those
streams. Each direction alternates a page and acknowledgement. The receiver
acknowledges only after committing the content tables and receive cursor in one
transaction. A lost acknowledgement or connection resumes from that committed
cursor in a later session. A recreated database has a fresh stream ID and empty
receive progress, and can catch up while retaining its device identity.

The change ledger retains the newest sequence for each record key. Every actual
content mutation, including imported changes and redaction/tombstones, receives
a durable sequence independent of wall-clock conflict versions. Local edits
behind an earlier cursor therefore appear after it. The session's fixed upper
sequence excludes incoming changes and later local edits until the next round.
Recently imported records can be echoed once; after both cursors catch up,
unchanged records are omitted. Receiving a peer's records is not permanent proof
that it will retain them after a database replacement.

Pages contain at most 32 records and fit the existing 16 MiB message bound.
SQLite byte checks precede loading a record, JSON escaping is counted without
allocating an encoded copy, and the network encoder stops at its message cap.
The existing frame, connection, progress, absolute session and worker limits
remain. A session commits at most 1,024 alternating page pairs. Outgoing
background sync retains further work through the bounded scheduler; failures
retain committed progress for retry. Partial progress is reported as such.
Individual records that cannot fit a bounded message are preserved locally and
produce an explicit error. Their segmentation and large slow-link acceptance
remain release-audit obligations, recorded in the findings matrix.

Each database read/apply checks the captured policy epoch, enabled setting,
reset state and trusted/non-revoked peer. An active policy owner interrupts the
socket on loss of authority. The original opened database identity must still
match its path; removed or replaced databases cannot be reused by old sessions.
Malformed/non-progressing pages and concurrent stale cursors fail without
committing their content. Received text never enters unencrypted staging files.

Schema 7 installs the ledger, triggers, stream ID and receive table atomically.
An interrupted migration rolls back; retry preserves existing settings, words,
history, tombstones and keys. Older schema-6 builds refuse schema 7. Reset and
peer removal clear receive progress. An externally rolled-back database whose
sequence precedes acknowledged progress reports a re-pairing requirement; remove
and explicitly trust the peer again to clear that progress. Copying an active
WAL database as a standalone backup remains unsupported.

Deletion tombstones remain replicated so offline peers cannot resurrect removed
content. The ledger adds only key/progress metadata, not duplicate transcripts.
A safe distributed tombstone retirement policy is still an audit obligation;
no age-based cleanup is assumed to establish that every offline peer saw a
deletion.
