## 2026-10-23 · Speech cards play streamed previews as view state, restarting at the place (#875)

- **Previews are view state.** The run host keeps a job's latest preview in
  `JobView::preview` (`run_jobs::AudioPreview`: the take, the sniffed MIME, the BLAKE3 digest and
  the bytes), as it keeps streamed text in `partial`. It never reaches the document: no changeset,
  history entry, undo step or autosave. A retry drops it, and so does every end: a finished sound
  is played from its recorded result, and a failed or cancelled one leaves nothing to play, so the
  recent jobs hold no audio. The digest is computed on the job's thread, before the host's lock.
- **Bounded memory.** Only the latest preview is kept per job, and only bytes that sniff as audio,
  at most 32 MiB (`MAX_PREVIEW_BYTES`, the largest speech reply read). The player keeps only the
  preview a card shows and the one being imported to replace it; a superseded preview is dropped
  as soon as nothing shows it, and previews never get a waveform.
- **Asked for only where it plays.** `RunHost::with_previews` turns on `stream_previews`. The
  desktop app's runner asks; the CLI and the browser (which can't play sound yet) don't, so they
  send speech exactly as before.
- **Whole files, played through the existing player.** A growing MP3 is not decoded as it grows:
  each preview is a complete, if truncated, MP3, imported through ffmpeg like any sound and played
  by the same feeder. When a card's sound changes from a preview to its job's next preview, or to
  the result that job finished with, the card keeps its place and, if it was playing (or had played
  to the end of a preview and was waiting for more), plays on from there. The old sound plays on
  until the new one is imported, so the switch costs a restart of the feeder at the current
  position, not a gap while ffmpeg works. Any other change of sound starts again from the start,
  paused, as before. This is the simplest robust approach; decoding a growing stream incrementally
  would need a decoder of our own or a long-lived ffmpeg pipe per card.
- **No preview equal to the reply.** ElevenLabs previews the audio before a chunk only once that
  chunk arrives, so the last chunk never makes one, and a transport that can't stream (which hands
  the whole reply over as one chunk) makes none.
- **Speech cards keep room for their sound.** A Generate audio card always reserves its sound area
  (saying "No sound yet" until it has one), so the card doesn't jump when the first preview plays.
- **Takes, not just jobs.** A preview names its take (`run_jobs::Take`: the job and its attempt),
  and a finished job says what it made (`JobView::made`: the attempt and its result's media). A
  card plays on only from one take's preview into the same take's next preview, or into the
  sound the card now shows when that take made it. A retry is another take, so it starts again
  from the beginning, and a result that is held (the card still shows its earlier sound) starts
  that earlier sound afresh rather than continuing it from the new take's place.
- **Nothing plays on unseen.** When a job ends or tries again and its card has no sound of its
  own to show (or, zoomed out, none its job made), the card's player lets the preview go: it
  stops, and its bytes are dropped.
- **Known edge.** A preview whose header claims the whole length (an MP3 with a Xing header
  written up front) shows that length; it plays to where its bytes end and waits there for the
  next preview.
