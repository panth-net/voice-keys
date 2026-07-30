# Tray icons

Voice Keys renders its tray icons procedurally in Rust — see `make_tray_icon`
in `src/main.rs`. Nothing in this directory is loaded at runtime today.

It exists as the intended home for hand-drawn replacements, one per visual
state:

- `idle.svg`
- `recording.svg`
- `processing.svg`
- `finished.svg`

Wiring them up would mean rasterising each SVG to RGBA at tray resolution and
returning that from `make_tray_icon` instead of the generated bitmap. If you
want to take that on, open an issue first: the procedural icons are
deliberately dependency-free, and pulling in an SVG rasteriser is a tradeoff
worth agreeing on before the work happens.
