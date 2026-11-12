# FigCraft app icon

The final FigCraft brand mark supplied by the owner: a colourful rounded "F" on a dark navy field,
cropped tight around the letter and clipped to a rounded square (corner radius 22 % of the side).

## Files

- `figcraft-<size>.png` (16, 32, 48, 64, 128, 256, 512, 1024): renders of the tile; `packaging/macos/package.sh`
  builds the `.icns` from them.
- `figcraft.ico`: Windows icon, 16/24/32/48/64/128/256 px (PNG entries). `winresource` embeds it through
  `apps/figcraft/build.rs` when `rc.exe` exists; otherwise `build-app.bat` (repo root) stamps it with `rcedit`.
- `hicolor/<size>/apps/ai.storyteller.figcraft.png` (16-512): Linux icon theme; the 256 px one is also the
  runtime window icon off macOS.
- `figcraft.svg`, `figcraft-small.svg`, `hicolor/scalable/...svg`: SVG wrappers around the 512 px render (the
  mark is raster artwork, not a vector).

The original 1254 px artwork is not kept in this repo. The icon is the owner's own artwork.
