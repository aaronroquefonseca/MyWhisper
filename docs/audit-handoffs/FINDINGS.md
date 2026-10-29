# Complete finding ownership

All 51 groups are assigned. Historical audit remains `08d2f6ae1295cf088cb8bb9fc12ea38fd97a4186`. Every grouped requirement is retained in the complete original matrix. No fully closed count is asserted for the new drafts.

| Finding | Primary continuation | Secondary scopes |
| --- | --- | --- |
| R01: Sync off still allows transcript transfer | #18 |  |
| R02: Outgoing sync does not enforce local peer trust | #18 |  |
| R03: Deleted transcripts retain their original content and are exported | #18 | #22 |
| R04: A stale discarded record resurrects deleted history | #18 |  |
| R05: Reset retains trusted peers, enables sync, and permits deleted data to return | #18 | #20, #22, #23 |
| R06: First-run identity creation is not atomic | #15 | #18 |
| R07: First-run database-key creation can lose the key to an encrypted database | #18 |  |
| R08: Release/cancel can arrive before the queued press installs its session | #22 | #19 |
| R09: An old release disarms the next recorder | #22 |  |
| R10: Canceling speculative recordings permanently occupies workers | #22 |  |
| R11: Unicode dictionary prompts panic; a panic can leave dictation permanently busy | #17 | #22 |
| R12: An unauthenticated discovered device ID can panic the GTK UI | #18 | #23 |
| R13: WebSocket setup and TLS streaming can block indefinitely | #22 |  |
| R14: Multichannel CPAL input is treated as mono | #19 | #22 |
| R15: Server port and exposure edits do not invalidate running-server identity | #22 |  |
| R16: Default sync listener has no connection/concurrency budget | #18 |  |
| R17: Raw and final transcripts are written outside the encrypted history store | #22 | #17, #23 |
| R18: Process adoption/reaping can target unrelated applications | #22 |  |
| R19: LWW comparisons are not atomic with writes | #18 |  |
| R20: Wall-clock LWW loses fast local edits and traps future-dated data | #18 |  |
| R21: Invalid or older settings JSON silently resets every preference | #18 | #23 |
| R22: Database migrations lack a coordinated, recoverable boundary | #18 |  |
| R23: Hotkey cancellation/muting/reconfiguration loses session state | #19 |  |
| R24: Shortcut capture does not suspend the service listener | #19 | #23 |
| R25: Legacy custom shortcuts migrate to F13 | #18 | #19 |
| R26: Shortcut parsing, conflict detection, and inverse key mapping disagree | #19 |  |
| R27: Held keys are merged across input devices and survive disconnects | #19 |  |
| R28: Capture and inference failures lack actionable user feedback | #22 | #19, #23 |
| R29: Whistle FIFO/process finalization has unbounded and incomplete paths | #22 |  |
| R30: Speculative finalization can drop unprocessed tail audio | #22 |  |
| R31: Speculative agreement confuses rolling-window and full-prefix text | #22 |  |
| R32: Delivery is outside the busy guard and can overlap or reorder | #21 | #22 |
| R33: Capture memory and streaming copying are unbounded | #19 | #22 |
| R34: Server readiness and keep-warm ownership are insufficient | #22 | #20 |
| R35: Cohere is selectable/downloadable but cannot be used | #22 |  |
| R36: Self-hosted WebSocket configuration drops authentication and accepts incompatible routing | #22 |  |
| R37: Claude cleanup thinking requests are invalid and response parsing misses text | #17 | #22 |
| R38: Cleanup accepts truncated or noncanonical replies as complete dictation | #17 | #22 |
| R39: Audio retention differs by engine and ignores a changed privacy preference | #22 | #18, #21, #23 |
| R40: Retained audio and temporary files are not protected like the database | #22 | #18, #20 |
| R41: Retention maintenance only runs during successful delivery | #18 | #23 |
| R42: Sync restart and address/discovery handling are unreliable | #18 | #23 |
| R43: Pairing and gossip need durable revocation and bounded reachability metadata | #18 |  |
| R44: Full-state synchronization scales poorly and applies partial rounds | #18 |  |
| R45: Clipboard copy reports success before the command succeeds; restoration is incomplete | #21 | #23 |
| R46: GTK signal closures retain dictionary widgets after removal | #16 | #23 |
| R47: Opening Settings changes the active remote engine's language | #23 |  |
| R48: Release packaging and prerequisite handling are incomplete | #15 | #23 |
| R49: Download cancellation, integrity, and installation are incomplete | #20 | #22, #23 |
| R50: Settings/state boundaries and documented architecture have drifted | #22 | #18, #23 |
| R51: Release automation has no stable-version gates or artifact integrity checks | #15 | #20, #23 |

Additional deletion-toast correction is already merged in #6. The System-theme/Tela inspection issues were evidence-backed launcher/environment determinations; later GTK 4.22 fallback/logo defects are merged in #6/#7. Final physical/package integration remains owned by #15/#23.

Stack: **#18 → #19 → #20 → #21 → #22 → #23**. #15/#16/#17/#18 each target main. #15 coordinates current main release/upgrade and all final acceptance. #16/#17 are the closest standalone code continuations.

Existing open #11 overlaps server status/switching in #22; preserve its independently reviewed behavior when integrating. Existing #4 is a separate Qwen feature; these drafts do not overwrite or claim to implement that feature.
