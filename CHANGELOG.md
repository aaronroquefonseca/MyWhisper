# Changelog

## 0.9.1

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
