## 2026-10-27 · Timeline export shares the cancellable file worker and emits bounded fade envelopes (#643)

- **Keep programmatic compatibility.** `timeline.export_interchange` keeps `canvas`, `id`,
  `path`, `overwrite` and its synchronous default `{bytes}` result. An optional `background`
  flag matches `history.export`; the app opts in. Its initial `{pending:id}` is an admission
  receipt, not a fabricated successful byte count. `export.status`/`export.cancel` and the
  successful terminal `{state:"exported",bytes}` use the existing export protocol.
- **Capture metadata, authenticate on the worker.** An owned bounded timeline recipe and cheap
  asset snapshot retain no whole project/history clone. Admission reads no asset body. The
  worker builds XML with cancellation checks, then acquires one private `open_export_verified`
  lease per source and streams it into the destination's atomic ZIP stage. It releases that
  source before the next. The existing process-wide worker slot, workspace quota, SaveRunner,
  retained receipt, session identity, quit guard and publication boundary remain authoritative.
  There is no inline fallback or all-source staging in anonymous system temp.
- **Bound both representation and work.** At most 256 media, 8 GiB of aggregate media and
  16 MiB per XML/report/SRT sidecar are admitted. Fade output admits at most 65,536 keyframes
  per document before unbounded allocation. Cancel checks run during envelope construction,
  sidecar chunks and media chunks; an OS call already in progress must return first.
- **Use original source-time envelopes.** Picture opacity uses `adjust-blend amount`; sound
  uses `adjust-volume amount`. The param/keyframe form follows Apple's primary
  [adjustment](https://developer.apple.com/documentation/professional-video-applications/adjustment-attributes-and-effect-parameters)
  and [animation](https://developer.apple.com/documentation/professional-video-applications/animation)
  documentation. Frame-grid source times preserve trims and the full dissolve handles.
  Picture corners represent linear opacity; audio samples linear amplitude on every sequence
  frame before conversion to dB, rather than interpolating a near-silent four-point dB ramp.
  Non-default opacity is the picture's peak. Muted/zero audio is -96 dB throughout. Frame
  rounding remains a loss. Audible fades report the interframe interpolation approximation
  separately from the removed omitted-fade loss. Unsupported playback speed still reports its unretimed length;
  played fade durations scale to that same source clock.
- **Keep actual editor validation as a merge blocker.** Neither Final Cut Pro nor Resolve is
  installed here. XML goldens, mock tests and original sample archives do not satisfy the
  actual-import acceptance criterion. The PR remains draft without issue closure until both
  app versions and observed import behavior are recorded in [the import matrix](../timeline-interchange.md).
  In particular, destination interpolation between audio frame samples is not claimed to be
  sample-identical. Apple's DTD is not vendored without a permissive redistribution license.
