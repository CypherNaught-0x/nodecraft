## 2026-10-23 · Saved 3D turntable videos (#575)

- **One Mesh input, one ordinary Video output.** `mesh.turntable` reuses the viewer's
  `MeshCapture` renderer and the existing bounded `VideoService` raw-frame encoder. The viewer's
  Turntable button dispatches `mesh.capture` with shot `turntable`; the command creates the
  wired action and saves its camera/style as one undo step. Moving the viewer later changes
  no saved capture parameters. Like other video actions, it waits for an explicit run rather
  than rendering during live preview. Runs need the native app and configured FFmpeg/ffprobe.
- **A repeatable half-open orbit.** A full turn samples `start + direction * 360 * i/N`.
  Back-and-forth uses a triangular wave over the directed arc from first to second angle;
  clockwise chooses the clockwise arc, including the long arc when appropriate. Even frame
  counts include the far endpoint once. Both modes omit the repeated loop endpoint. Duration
  rounds to the nearest whole frame; encoded duration is exactly frame count / rate.
- **Bound work before opening tools or decoding.** Even sides are 16-2048, rate is 1-60 fps,
  duration 0.1-60 seconds with at least two frames, and width * height * frame count is at
  most 268,435,456 pixels. Angles, elevation, zoom and colours are checked before GPU work.
  This is an aggregate work bound, not a promise about process RSS.
- **Frame memory stays independent of duration.** One reusable GPU target, one CPU readback
  and the media service's two-frame queue replace a vector of all frames. The existing media
  supervisor bounds encoder memory, encoded file size and stderr; cancellation/drop signals
  the supervisor, which kills/reaps the encoder and joins pipe workers. The encoded file uses
  the shared run staging quota before readback allocation. The output is soundless WebM;
  transparent pixels are composited over black, as in the existing frame-to-video actions.
- **Keep capture close semantics.** Stack-owned GPU devices and process-wide serial admission
  stay under the existing capture coordinator. Cancellation is checked before each frame and
  during encoder backpressure/readback. The whole active turntable shares a forty-second
  deadline, including opening tools, decoding, GPU readback, encoding and result readback;
  oversized/slow work fails with guidance to lower size, duration or rate. The existing
  forty-five-second close/drain budget and stuck-driver limitation remain; see
  [capture lifetime](2026-10-22-capture-close-lifetime.md).
