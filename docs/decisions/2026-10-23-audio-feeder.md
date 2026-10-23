## 2026-10-23 · Sound playback advances without app frames (#666)

- A sound host lazily starts one device/PCM feeder thread. It owns device opening, closing,
  decoding jobs and wall-clock retries. The device callback consumes the existing bounded ring;
  neither needs an egui frame. The UI reads a progress snapshot when frames resume.
- The request mailbox holds only the latest play/seek request. Seeking cancels old decoding,
  resets the ring and previews drag positions without decoding until release. Committed seeks
  wait for 75 ms of stable input before decoding again. Playing another card
  closes the previous output before opening another; changing settings stops and reuses the
  same worker. There is never a new thread per click, seek or settings change.
  A global pointer release also commits a drag if its card left the viewport; if app frames stop
  before release, or the pointer stays still while held, the worker resumes the last target after
  two seconds without drag updates.
- Native device opening and closing cannot be interrupted through cpal. They run outside app
  frames, with no mailbox lock held. Cancellation remains responsive; a blocked native call may
  retain this one worker until the OS returns. Dropping the host signals shutdown and never joins
  from an app frame. A stopped callback stream is detected after three seconds without callbacks,
  even when the backend reports no error.
- An opening device without a report after three seconds shows an actionable notice. The UI
  stops that request; a late OS return cannot revive it. A blocked native call never creates a
  replacement worker. Only a worker whose panic has fully unwound may be replaced on the next
  play. Idle and terminal epochs park on a condition variable until a request or shutdown.
- Device selection prefers stereo 48 kHz F32/I16, then other supported F32/I16 rates, then the
  backend default. The callback increments an atomic heartbeat without allocation or clock reads.
- A reported duration is a playback hint. PCM requests continue to decoded EOF, at most two
  seconds per request and no further than the existing one-hour accepted audio limit. This fixes
  understated VBR durations without extending an untrusted input's allocation/process bounds.
  Observed playback length is view state; no document metadata or undo entry changes.
- Fake decoder/output tests exercise the actual worker without app frames, including playback
  beyond eight seconds, underestimated duration, cancellation, seek coalescing, blocked device
  opening and silent device failure. Installed-tool/device tests are separate opt-in evidence;
  wasm continues to show the existing desktop-playback guidance.
