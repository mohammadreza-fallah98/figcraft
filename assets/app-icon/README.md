# FigIran app icon

The final FigIran brand mark supplied by the owner: a colourful rounded "F" on a dark navy field,
cropped tight around the letter and clipped to a rounded square (corner radius 22 % of the side).

## Files

- `figiran-<size>.png` (16, 32, 48, 64, 128, 256, 512, 1024): renders of the tile; `packaging/macos/package.sh`
  builds the `.icns` from them.
- `figiran.ico`: Windows icon, 16/24/32/48/64/128/256 px (PNG entries). `winresource` embeds it through
  `apps/figiran/build.rs` when `rc.exe` exists; otherwise `build-app.bat` (repo root) stamps it with `rcedit`.
- `hicolor/<size>/apps/ai.figiran.app.png` (16-512): Linux icon theme; the 256 px one is also the
  runtime window icon off macOS.
- `figiran.svg` (full: tile, glow), `figiran-small.svg` (simplified for tiny sizes), `hicolor/scalable/...svg`: hand-drawn vector
  redraws of the mark (viewBox 0 0 1024 1024); `cargo xtask icon` renders the rasters from `figiran.svg`.

The original 1254 px artwork is not kept in this repo. The icon is the owner's own artwork.
