# MorReel Studio

Portrait-only (9:16) video editor for phone reels. A Rust/Dioxus desktop UI over
the system `ffmpeg` — the same shell-over-engine split kdenlive (MLT) and
openshot (libopenshot) use, minus the C++ binding layer.

Everything you scrub is what you ship: preview frames, effects, titles, and the
export all run through the same ffmpeg filter chains at 1080×1920, 30 fps.

## Features

- **V1 main track** — trim, reorder, split at playhead, per-clip speed
  (0.25×–4×) and volume; every delete is a ripple delete by construction (a
  concat timeline has no gaps to leave).
- **Photos on the timeline** — drop a PNG/JPEG onto V1 or V2 and it loops for
  as long as you hold it, so the Motion effects become camera moves over a
  still. Same lanes, same effects, same export.
- **V2 overlay track** — full-frame B-roll cutaways; main audio keeps playing.
- **T title track** — drawtext-rasterized title cards with an optional
  **cameo/intaglio bevel** (the Krita-derived mor_cameo_emboss algorithm).
- **A1 audio track** — music/VO mixed under with per-item trim and volume.
- **Effects** — B&W, Sepia, Warm, Cool, Punch, Dreamy, Vignette, and the
  Motion set (Slow/Pulse zoom, Drift, Sway) ported from
  [moranima](../moranima)'s camera moves; one ffmpeg filter each, identical in
  preview and export, with a strength slider that interpolates to identity.
- **Safe-area guides** (`G`) — shaded bands showing where a phone app's own
  header, action rail and caption block sit over the frame. Worst case across
  TikTok / Reels / Shorts, so clearing them clears all three.
- **Upload-length warnings** — the status bar flags when the reel outgrows
  Shorts (60 s), Reels (90 s) or TikTok (10 min).
- **Undo/redo** (`Ctrl+Z` / `Ctrl+Shift+Z`) — whole-timeline snapshots; a
  single slider drag collapses into one step.
- **Projects** — save and reopen an edit as a small JSON file (`.morreel`).
  It records the edit, not the media: sources stay referenced by path, and
  thumbnails, proxies, waveforms and title PNGs rebuild on load.
- **Snapping** — dragged overlays, audio and titles snap to clip cuts, the end
  of the reel, and the playhead.
- **Proxies** — background 480p scrub proxies (content-addressed cache) for
  smooth seeking; export always uses the originals.
- **Playback** — in-app silent proxy playback (Space), or a fast preview
  render handed to mpv/ffplay for full fidelity with audio (Ctrl+P).
- **Desktop chrome** — menu bar, keyboard shortcuts, frameless/native/tiling
  window modes. Mobile (Android/iOS) builds swap the timeline for a clip pager.

## Building

Requires Rust, `ffmpeg`/`ffprobe` on PATH, and the sibling
[`mor_rust_dioxus_ui_kit`](../mor_rust_dioxus_ui_kit) crate (path dependency —
check out both under a common parent, or adjust the path in `Cargo.toml`).

```bash
cargo run              # desktop app
cargo test             # unit + end-to-end ffmpeg smoke tests
MORREEL_MOBILE=1 cargo run   # preview the mobile layout on desktop
```

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).

Copyleft lineage: `src/bevel.rs` is the Intaglio/Cameo bevel from
wearable-dictionary-designer (GPL-2.0-or-later), itself derived from Krita's
`kis_ls_bevel_emboss_filter.cpp` by Dmitry Kazakov via the mor_cameo_emboss
GIMP plugin. That file remains GPL-2.0-or-later; the combined work is
distributed under GPL-3.0-or-later, in the same family as the projects this
editor takes inspiration from (kdenlive, OpenShot, smplayer). FFmpeg is
invoked as an external process, not linked.
