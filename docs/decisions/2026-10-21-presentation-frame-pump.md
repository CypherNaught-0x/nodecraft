## 2026-10-21 · Presented frames come from a vsync frame pump (#526)

The `presentation` metric fills from **frame-pump timing**, not from an OS present-rate source.

- **How.** While a capture runs and the host has turned the pump on
  (`NodeCraftApp::set_frame_pump`), every frame calls `request_repaint`, so the next frame starts
  as soon as eframe returns. eframe presents with `PresentMode::AutoVsync` (its default), so
  `get_current_texture`/`present` wait for the display: the time between two pumped frame starts
  is one presented interval. Only intervals whose previous frame was pumped count, so idle redraws
  and the first frame after the pump starts are never presented samples; nor is a second egui
  pass after a discard, which shares its frame's present.
- **The host names its source.** `Recorder::set_presentation_source(Some(VsyncFramePump))` is the
  only way `presentation` takes samples, and the report says which source it was
  (`presentation_source`). A host that can't promise vsync passes `None` and the metric stays
  unsupported with `presentation_timestamps_not_available`. The desktop's `--perf-run` names the
  source only when the configured present mode is `AutoVsync` or `Fifo` (the configured mode, not
  one read back from the surface; `AutoVsync` falls back to FIFO, which every surface supports). Normal use, the CLI
  and wasm don't pump, so they keep the explicit reason.
- **Dropped frames** are counted against 60 Hz: an interval within 25 ms (1.5 × 16.7 ms) is
  on time; a longer one dropped its length rounded to whole frames, minus one (a 41 ms interval
  dropped one frame, a 42 ms one two). The report adds the sum
  (`dropped_frames`) and the longest run (`longest_drop`). The schema is now `nodecraft.perf/3`,
  because the envelope gained fields.
- **Why not an OS source.** eframe 0.36 exposes no present timestamps or refresh rate, and the
  OS APIs that do (CVDisplayLink, DXGI frame statistics, `wp_presentation`) have no safe,
  permissively licensed wrapper that fits without `unsafe`. Pumping needs nothing new.
- **What it doesn't prove.** On a display faster than 60 Hz the intervals are shorter than the
  60 Hz budget, so the result shows whether 60 fps holds, not the display's own rate. A compositor
  that delays presenting a hidden or covered window shows up as drops; run the window on
  screen. The pump adds one `request_repaint` per frame and no allocation.
