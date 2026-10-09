# UI/UX audit and refactor

MyWhisper is a native desktop dictation app. The central workflow is: choose a microphone and transcription engine, dictate into another app with a shortcut, optionally clean up the transcript, then paste/copy the result. History, dictionary terms, and trusted-device sync support that workflow.

## Audit findings

| Original friction | Implemented change |
| --- | --- |
| Settings and Sync appeared as large overlays, unlike the other destinations. | Five persistent destinations: Dictation, Activity, Dictionary, Devices, Settings. Settings has its own task-oriented subnavigation. |
| Engine terminology exposed implementation details before explaining the task. | Transcription and Text cleanup explain their order, fallback behavior, and whether audio or text goes to a cloud provider. |
| Advanced options competed with everyday preferences. | Custom prompts, remote connection forms, recording-indicator details, server/network options, and speculative decoding expand on demand. |
| Copy implied dictionary terms were guaranteed and snippets were typed shortcuts. | Words help supported engines recognize terms. Spoken trigger phrases insert saved text; comma-separated alternatives remain supported. |
| Edits could disappear during background dictionary refreshes; snippet edits flattened multiline content. | Refresh waits while editing; both snippet creation and editing use multiline text fields. |
| Success/failure was often visible only in logs. | Action feedback appears in native toasts or contextual status. Busy operations disable their buttons. |
| Destructive controls removed content immediately. | Native confirmations explain the consequence, default to Cancel, and distinguish configuration removal, download deletion, history deletion, unpairing, and reset. |
| Activity totals depended on the latest 200 rows and speed included unmeasured recordings. | Totals query all retained history. WPM uses recordings with measured duration. Labels explicitly describe saved history and the current streak. |
| Bulk history deletion and retention could be undone by syncing an older peer. | These deletions now retain sync tombstones, matching individual deletion. |
| Local date grouping, heatmap alignment, language rebuilding, and engine editing had inconsistencies. | Dates use local calendar days; heatmap labels match the displayed cells; language changes retain the destination; editing clones configuration before emitting dependent widget signals. |
| The GUI never filled its nearby-device discovery state. | A browse-only mDNS session populates Nearby while sync is enabled; the service continues to own advertising and inbound connections. |

## Design decisions and internet references

These are adaptations to this app's actual behavior, not promises of another app's feature set.

| Decision | Reference and rationale |
| --- | --- |
| Persistent sidebar with a clear selected destination; ordinary pages for settings/devices. | [GNOME sidebars](https://developer.gnome.org/hig/patterns/nav/sidebars.html) provide the native navigation pattern for peer destinations. |
| Neutral light/dark surfaces, a single blue accent, and System/Light/Dark choices. | [GNOME UI styling](https://developer.gnome.org/hig/guidelines/ui-styling.html) supports consistent native light/dark styles and an explicit system preference. The exact palette is MyWhisper's own. |
| System font, relative type sizes, restrained heading hierarchy, readable secondary text. | [GNOME typography](https://developer.gnome.org/hig/guidelines/typography.html) supports inheriting system fonts and limiting type variations. |
| Related preferences grouped into bounded cards; detailed forms collapsed. | [GNOME boxed lists](https://developer.gnome.org/hig/patterns/containers/boxed-lists.html) informed grouping related controls. [Superwhisper essential settings](https://superwhisper.mintlify.app/get-started/essential-settings) informed prioritizing the microphone, shortcut, and dictation setup. |
| Searchable recent dictations, final text first, original text in a secondary disclosure. | [Superwhisper History](https://superwhisper.mintlify.app/get-started/history) informed the distinction between finished output, original transcription, and history metadata. MyWhisper search currently covers the latest 200 recordings. |
| Separate Words and Snippets, persistent field labels, duplicate validation, multiline expansions. | [Wispr Flow dictionary](https://docs.wisprflow.ai/articles/4052411709-Teach-Flow-your-words-with-the-dictionary) clarifies recognition vocabulary. [Wispr Flow snippets](https://docs.wisprflow.ai/articles/5784437944-create-and-use-snippets) informed the spoken-trigger/expansion model. [GNOME text fields](https://developer.gnome.org/hig/patterns/controls/text-fields.html) informed labeling and validation. |
| Verbs describe the actual action; primary actions stand out; unavailable actions are disabled. | [GNOME writing style](https://developer.gnome.org/hig/guidelines/writing-style.html) and [buttons](https://developer.gnome.org/hig/patterns/controls/buttons.html) informed concise labels such as Add word, Send request, Start service, and Capture keys. |
| Per-record secondary actions live in a More menu; destructive confirmation defaults to Cancel. | [GNOME menus](https://developer.gnome.org/hig/patterns/controls/menus.html) and [dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html) informed action placement and explicit confirmation. |
| Persistent service state uses a banner; completed actions use transient feedback. | [GNOME toasts](https://developer.gnome.org/hig/patterns/feedback/toasts.html) distinguishes individual events from ongoing state. |
| Device identity, nearby devices, pairing requests, and paired devices are separate concepts. | [Signal linked devices](https://support.signal.org/hc/en-us/articles/360007320551-Linked-Devices) informed making the other device and trust decision explicit. MyWhisper retains its existing ID-based, direct device-to-device protocol. |
| Ollama's action says Apply to model, shows busy/results, and avoids generating a dummy response. | [Official Ollama API](https://github.com/ollama/ollama/blob/main/docs/api.md) documents loading/unloading with empty chat messages and `keep_alive`. This action applies the selected residency setting; Add/Save persists the connection configuration. |

## Visual system

Shared layout and component rules live in `src/app.css`; dark mode overrides palette values in `src/app.rs`.

| Token | Light | Dark |
| --- | --- | --- |
| Background | `#f7f8fa` | `#1b1e24` |
| Card | `#ffffff` | `#242830` |
| Primary text | `#202631` | `#edf0f5` |
| Secondary text | `#5b6472` | `#aab3c2` |
| Accent | `#315fcb` | `#91b4ff` |
| Selection | `#e5ecfc` | `#2d3c58` |

The system font is inherited. Body copy uses the native size, titles use `2em`, section headings `1.15em`, and secondary captions approximately `0.93em`. Page padding is 28px; related controls use 12–16px spacing; cards have 12px corners. Focus outlines, persistent labels, selectable text, wrapping descriptions, and accessible names complement color cues. This remains a desktop layout, with vertical scrolling and a horizontally scrollable heatmap where needed.

## Control and behavior map

### Window and navigation

- Header controls provide normal drag, minimize, maximize, and close behavior.
- Sidebar buttons switch destinations without modal dismissal. The selected destination remains visible.
- Settings subnavigation switches the seven preference sections. Dictation setup links select the relevant settings section directly.
- Theme applies immediately and persists. Interface language rebuilds the window at the current destination and section; repeated app activation presents the active window.

### Dictation

| Control | Behavior |
| --- | --- |
| Start service | Requests `systemctl --user start mywhisper.service` asynchronously; reports command success/failure. Heartbeat determines the displayed running state. Running does not guarantee a configured engine or working microphone. |
| Enable shortcuts | Persists the global shortcut enable flag. The service observes preferences. |
| Set up dictation / Edit shortcuts / History settings | Opens Transcription / Shortcuts / Privacy & storage respectively. |
| Search recent dictations | Filters final and original text, case-insensitively, in the latest 200 loaded recordings. |
| Show canceled recordings | Includes discarded recording metadata in the list. Discarded recordings have no transcript to copy. |
| Copy | Copies the finished text using the configured clipboard backend and reports its result. |
| More → Show original / Hide original | Expands/collapses the original transcription, preserving disclosure state during live refresh. Identical original/final text is described without inferring whether a model ran. |
| More → Delete | Confirms, tombstones the history record, hides the successful deletion, and requests sync. |

Empty states distinguish no history yet, history saving disabled, and a search without matches. Shortcut instructions describe Hold, Toggle, or Smart behavior rather than assuming every shortcut requires holding.

### Activity

There are no mutation controls. Words and dictations count retained, non-deleted, non-discarded records. WPM excludes recordings without measured duration. Current streak describes consecutive active local calendar days. The 24-week chart uses retained activity; hovering a cell shows its actual date and word count. Deleting history changes these totals.

### Dictionary

| Control | Behavior |
| --- | --- |
| Words / Snippets tabs | Switch between recognition vocabulary and spoken text replacement. |
| Add word / Enter | Trims and validates the word or phrase; rejects empty and case-insensitive duplicate entries; saves, clears the field, and returns focus. |
| Word Edit → Save / Enter | Validates and saves the replacement before deleting the original. |
| Word Edit → Cancel | Discards the draft. Background updates wait during editing. |
| Create snippet | Requires at least one nonempty comma-separated trigger and expansion text; rejects overlapping trigger aliases case-insensitively; preserves multiline text. |
| Snippet Edit → Save / Cancel | Uses the same validation and a multiline editor; saves the replacement or discards the draft. |
| Delete word/snippet | Confirms deletion, updates the settings mirror, and requests sync. |

Words bias engines that support dictionary hints; they cannot guarantee recognition. Snippets match spoken transcript phrases and are applied by the existing normalization pipeline.

### Settings

Ordinary preferences save automatically. Add/Save forms commit connection configurations explicitly; Cancel abandons them. Dropdowns retain their backend values and supported options.

| Section/control family | Behavior |
| --- | --- |
| General: sound, media, microphone | Chooses feedback sounds, media pause/resume, input device, readiness duration, and extra listening delay. Disconnected selected devices remain visible. |
| General: recording indicator | Chooses initial position, animation, bars, height, visual theme, ornament, and idle animation. Details expand on demand. |
| Shortcuts: Add | Parses the entered key combination, assigns Hold/Toggle/Smart behavior, persists it, and updates bindings. Invalid input and equivalent duplicate combinations stay visible with an explanation. |
| Shortcuts: Capture keys | Starts key capture; the existing controller records supported keys/modifiers into the new shortcut field. Capturing keys is distinct from adding the binding. |
| Shortcuts: binding mode / Remove / Reset | Changes the activation mode, removes a binding, or restores the default F13 Hold binding. The last binding stays because the backend otherwise restores F13; use Enable shortcuts to pause. Smart timing sets the tap/hold threshold. |
| Transcription priority: enable / move up/down | Controls top-to-bottom fallback. Movement beyond the list bounds is disabled. |
| Transcription: Add on-device engine | Restores the Local option to the priority list if absent; it uses the local provider/model configuration below. |
| Transcription: Add remote engine / Edit / Save / Cancel | Configures self-hosted HTTP/WebSocket or supported cloud connections. Required connection fields and server URL schemes/hosts are validated. API keys remain masked. Streaming is enabled only for compatible WebSocket endpoints. |
| Priority: Remove | Confirms removal of the saved connection configuration; downloaded files and history remain. The last configuration stays because an empty list otherwise triggers legacy migration; use its switch to disable it. |
| Models: provider tabs / model and language selectors | Chooses the local provider, available model, and supported transcription language. Fixed-language engines explain why the language selector is disabled. |
| Models/runtimes: Download / Cancel / Retry | Starts, cancels, or retries the existing background download/install pipeline; shows progress and errors. |
| Models: Use / Using | Selects an installed model; Using identifies the selected or singleton model and is disabled. |
| Models/runtimes: Delete | Confirms removal of downloaded files and reports failure/success. Downloading again is required to use them. |
| Voice activity / live preview / speculative options | Sets detection threshold, compatible live-preview behavior, and speculative update/overlap timing. Capability badges describe unsupported dictionary or streaming features. |
| Servers/network: Start server / Stop | Sends the existing one-shot command to the service; server status follows process state. Auto-start, readiness, backend, port, and LAN exposure remain configurable. |
| Text cleanup: master switch / Custom prompt / Restore default | Enables optional cleanup, edits the persisted prompt, or restores the original prompt. |
| Text cleanup: priority / Add / Edit / Save / Cancel / Remove | Uses the same ordering and connection-form model as transcription. Ollama and supported cloud providers remain available. Managed local models are explicitly unavailable and cannot be added. |
| Text cleanup: Apply to model | Sends the selected keep-alive choice to the configured Ollama model; shows a busy state and outcome. It does not save the surrounding form. |
| Text delivery | Chooses automatic paste, clipboard retention, backend tools, paste combination, and the existing history visibility preference. Backend availability hints retain detected dependency details. |
| Privacy & storage | Configures audio/history saving, retention, and canceled-record metadata. Retention controls become insensitive when their corresponding saving option is off. |
| Privacy: Open folder / Clear all audio / Clear all transcripts | Opens the audio directory or confirms bulk deletion. History deletion retains tombstones for sync safety. |
| System: Check for updates | Runs the existing GitHub release check and updates contextual status; it does not install an update. |
| System: Open folder / Clear cache | Opens the model cache or confirms removal of downloaded models/runtimes. Preferences/history/identity remain. |
| System: Reset app data | Confirms deletion of application data and downloads, restores default preferences, and rebuilds the UI. Device identity and the running service are retained by the existing reset model. |

### Devices

| Control | Behavior |
| --- | --- |
| Enable device sync | Persists the master setting; the service watches changes. Turns local browsing on/off. |
| Discover this device | Controls advertising; disabled while sync is off. |
| This device disclosure | Reveals device name, immutable identity, configured port, and connection diagnostics. |
| Device name Save | Persists the friendly name; reports the result. |
| Device ID Copy | Copies the ID and reports the clipboard result. |
| Listen port Save | Accepts 0–65535; 0 retains the existing automatic-port meaning. Invalid input remains visible. |
| Nearby → Send request | Records the discovered address and attempts the existing pairing handshake. Discovery alone does not establish mutual trust. |
| Requests → Accept / Decline | Records the explicit trust decision. Accept attempts sync when the peer is available; both decisions show feedback. |
| Paired device trust switch | Changes this device's trust decision in the existing peer record. |
| Sync now | Runs a manual direct sync for a trusted peer with an address; disables duplicate requests while working; reports completion/errors. |
| Remove paired device | Confirms unpairing. Existing words, snippets, and history remain on each device. |
| Add device → Send request | Requires the other device's 64-character hexadecimal ID, rejects this device's own ID, and validates an optional manual `host:port`. A friendly name is optional. Without an address, the request waits for discovery/contact; sending a request is distinct from acceptance. |

### Recording overlay

The service retains its compact listening, live-preview, and processing states. The expanded result preview now has meaningful Dictation result, Copy, and Dismiss controls. Copy uses the displayed body; Dismiss hides the result. This expanded view is also used by the `pill` developer demonstration; this refactor does not claim a new service result-display workflow.

## Verification and reproducible preview

The preview uses a disposable in-memory store and sample dictations. It does not load the user's keychain/database or start the dictation service/microphone. Do not invoke service/server/download buttons unless intentionally testing those systems.

```sh
cargo run --example ui_preview -- home light en
cargo run --example ui_preview -- settings dark es 'Language Models'
cargo run --example ui_preview -- dictionary dark es Preferences empty
cargo run --example ui_preview -- home light en Preferences --smoke
```

The native smoke sequence exercises required/duplicate validation, Enter submission, edit preservation across a data refresh, Cancel/confirm deletion, multiline snippet creation/editing and alias conflicts, recent-history search, setup deep links, cleanup-model editing, bulk history Cancel/confirm, persisted theme changes, and language rebuilding against disposable data. It is run in English/light and Spanish/dark.

Visual inspection covers all five destinations and all seven settings sections, expanded forms, dark/light styles, English/Spanish, empty states, and narrower desktop windows. `cargo check --all-targets --locked` passes. `cargo test --locked` passes 76 tests, with five existing opt-in model/inference tests ignored. Tests cover translation-tag availability and placeholder parity, shortcut conflicts, provider/address validation, full-history statistics, and sync-safe deletion. Live microphone capture, downloaded inference runtimes, cloud credentials, real peer-device networking, and compositor-specific overlay placement need their normal hardware/integration tests.
