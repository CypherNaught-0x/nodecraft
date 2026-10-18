## 2026-10-18 · Performance capture (#151)

- **One recorder, in the session.** `engine::perf::Recorder` keeps the latest 4,096 samples per
  metric in microseconds. It is off until `perf.start`, and does no I/O and no allocation
  beyond those buffers. The session times every dispatch (plus undo, redo, load and save) while
  recording. The shell times frame preparation and the frame interval, and records the last
  frame's workload.
- **The host owns the clock.** `std::time::Instant` isn't available everywhere (wasm), so the
  shell installs a monotonic microsecond clock where it can. Without one, timings are reported
  as unsupported (`null`), never zero. A clock that runs backwards, or a sample over an hour,
  is counted as invalid, not kept.
- **A versioned report, nothing personal.** `perf.report` returns `nodecraft.perf/1`: per-metric
  nearest-rank p50/p95/p99, max, kept, recorded and invalid counts, frames over the 60 Hz
  budget, the workload, and build and platform labels. There are no document contents, prompts,
  assets, secrets or paths, and nothing is uploaded. The report says plainly that
  `frame_prepare` is CPU-only.
- **Capture is a command, not a document edit.** `perf.start`, `perf.stop`, `perf.clear` and
  `perf.report` take `{}`, so they show in the palette. They add no undo step, and the capture
  commands don't time themselves. They return a session-level effect (`Effect::Perf`), because
  command handlers can't change session state. Agents may run them.
- **Not here:** a GPU or presentation timestamp (#65), CI baselines and regression thresholds
  (#152), and a UI panel showing the report.
