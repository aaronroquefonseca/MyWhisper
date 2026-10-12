# Changelog

## 0.9.4

- Preserve device identity during legacy-key migration and concurrent
  startups. Protect private files with atomic writes, restrictive
  permissions, and checks against symlinks and non-regular files.
  ([#5](https://github.com/aaronroquefonseca/MyWhisper/pull/5))
- Fix symbolic logo visibility and toast contrast in light and dark
  themes, including close-button hover, pressed, and dismissal behavior.
  ([#6](https://github.com/aaronroquefonseca/MyWhisper/pull/6))
- Harden upgrades with stable-version selection, authenticated download
  checksums, archive validation, and service restart verification.
  Complete Nix build/runtime prerequisites, make CLI help and version
  available before storage initialization, and verify release versions,
  dependency notices, artifact integrity, and isolated regression/UI checks.
  ([#7](https://github.com/aaronroquefonseca/MyWhisper/pull/7))

## 0.9.3

- Refreshed desktop UI with persistent Dictation, Activity,
  Dictionary, Devices, and Settings navigation, a monochrome
  sidebar logo, light/dark themes, and clearer English/Spanish copy.
- Search recent dictations, inspect original transcripts, and use
  direct setup links; clearer service status, action feedback,
  validation, and confirmations for destructive actions.
- Separate Language models management from Text cleanup, with
  model fallback controls and a resizable prompt editor supporting
  explicit Save/Cancel, default restoration, and full-screen editing.
- Dictionary edits survive background refreshes; multiline snippets
  retain their formatting and reject conflicting spoken triggers.
- Activity totals use all retained history; speed excludes recordings
  without measured duration; local-date streaks and the responsive
  activity calendar display correctly in narrow windows.
- Devices shows nearby peers through mDNS browsing and clearer
  pairing, trust, and manual sync controls. Bulk history deletion
  and retention preserve tombstones so older peers cannot restore it.
- Restore automatic language detection for multilingual sherpa
  models. Ollama thinking defaults to off, including saved models
  without an explicit setting.
- Disposable UI preview and smoke checks for settings, history,
  dictionary, theme, language, and prompt editing workflows.

## 0.9.2

- Exact release-point capture: timestamped mic tap, truncate at
  release (nothing before cut, nothing after kept); fixed 350 ms
  headroom removed. Optional extra listening delay in Preferences
  (default off).
- Model-aware STT language list (single-language models lock,
  multi-language sherpa lists drop auto, default en); Cohere
  offline servers now get their required language flag.
- Sync engine applies immediately (toggle, port, name,
  autodiscovery); live listener port in the UI; passwordless
  automatic reachability check with firewall allow-hints instead of
  polkit queries; optional mDNS autodiscovery toggle.
- Settings System page: update status + manual re-check,
  consistent with the Home update notice.
- Pill live-preview race fix.

- Home shows an update notice when a newer private release exists
  (checked once per app launch via `gh`; install stays an explicit
  `update.sh` run).
- Install docs with copy-paste commands (Arch + NixOS, HTTPS flake
  input as the default).

## 0.9.0

First packaged release (`mywhisper` binary).

- Single supervised STT server (adopt/reap strays, autostart,
  lazy start with catch-up, keep-warm, Stop/Cold-start, live status
  including foreign servers).
- Servers section: Network subsection (single port override, LAN
  toggle), 350ms release headroom against tail clipping, cold-press
  records instead of aborting when the server is down.
- Self-hosted STT: bearer token, streaming toggle, `ws(s)://`
  sherpa-protocol plans (rustls-backed `wss://`).
- Sync tab: engine toggle (systemd owns the process). Home banner:
  service health + hotkey toggle. Live-preview race fix on the pill.
- Private release pipeline (Arch container CI), Arch PKGBUILD +
  `update.sh`, NixOS flake + user-service module, systemd user unit.
