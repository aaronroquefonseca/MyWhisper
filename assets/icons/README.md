# Icons — v5 initial

Motif: 5-bar waveform, center bar = speech bubble tail. Voice → text.

## Files
- `app-icon.svg` — source. Dark rounded tile `#0A111C→#202B3D`, gradient mark `#3EF2FD→#5B68FC`. 1254 viewBox. App launcher / store / about.
- `app-icon-preview-on-white-1254.png` — 1254px render on white bg. Preview only, not runtime.
- `symbolic.svg` — source. `currentColor`, no tile. Tray / toolbar / Noctalia. Recolors via CSS/context.
- `symbolic-preview-on-white-1254.png` — 1254px black-on-white render. Preview only.
- `symbolic-preview-on-white-24.png` — 24px black-on-white render. Small-size legibility check. Not a white icon despite old name.

## Use
- Launcher: `app-icon.svg`
- Tray/symbolic: `symbolic.svg`
- Previews: do not ship as assets, reference only.

## Orig names
Old `Downloads/voiceappicons/*-initial-v5*` were ref-only. Old `*-white.png` = white background, not white glyph. Renamed to `*-on-white-*` to fix.
