## 2026-10-22 · Captures own their devices and close drains every execution path (#603)

- A whole photo or view sheet owns its headless capture device on its worker stack, reusing it
  between the sheet's views. Process-wide admission still serializes captures. The device drops
  before the active-capture guard, on success, error and unwind; no process static retains it.
  This replaces the cross-capture cache and leaked-lost-device policy in
  [the original capture decision](2026-10-21-3d-capture.md). A new capture pays device setup again.
- A local debug smoke measurement ran the existing three-photo test in three fresh processes
  before and after this change. The cached-device version took 0.769–0.986 seconds per process
  (median 0.856); stack ownership took 0.631–1.481 seconds (median 0.656), with actual rendering
  confirmed in all new runs. Concurrent compilation and driver caches make these diagnostic
  timings, not a performance budget or a claim of improvement. Cross-capture cache reuse is
  deliberately traded for observable teardown; a sheet still reuses its device across views.
- Native close first cancels the app's live round and its run host's local captures. Cancelling
  the local work does not cancel a submitted paid provider job kept in the run journal. A live
  round gets its cancellation handle before its worker starts; queued work cannot start a late
  capture after close has already drained the GPU.
- Every capture is counted before model reads and decoding, including explicit runs and captures
  waiting for the serial admission. Close requests cancellation between views and refuses new
  admission while draining. An inactive slot means the capture's device has actually dropped,
  including a destructor which takes longer than the capture body.
- The capture close budget is **45 seconds total**: the 40-second whole-sheet readback allowance
  plus five seconds for teardown. The app uses the same deadline for its remaining live-worker
  join, before storage close and relaunch. Device creation, submission and driver destruction
  cannot be interrupted by safe Rust; if one never returns, close reports how many captures
  remain and gives up at the bound. It does not claim the driver has stopped or teardown was
  successful. A timed-out drain rejects admission until the last capture actually retires.
- Dropping a test Harness or a live round cancels only that round and joins its worker within
  the same bound; it never requests process-wide shutdown of unrelated apps. Worker panics are
  logged, including a compute panic before a finished handle is replaced by the next round.
- GPU-free controls inject blocked work and blocked device destruction, prove bounded give-up
  before releasing them, cover serially queued captures, and observe cleanup and panic counts.
  Actual capture tests continue to exercise the real renderer when a native GPU is available.
  A process-exit regression runs the existing three-photo test in three fresh processes, bounds
  and reaps them, and checks successful exit. Its CI step summary reports actual rendered photos
  separately from the existing no-GPU skip policy; skipped runs are not capture evidence.
