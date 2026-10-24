## 2026-10-23 · Streamed speech previews are a runner opt-in, few and whole-result-preserving (#671)

- **Previews are asked for, like streamed text.** `RunLimits::stream_previews` (off by default)
  makes the runner's `SubmitContext` collect previews; an adapter whose synchronous submit can
  stream reports the latest through it, and the runner turns each into
  `RunUpdate::Progress(JobProgress::Running { partial })`. A newer preview replaces one not yet
  reported. Without it, ElevenLabs speech is sent exactly as before.
- **Few previews, bounded copies.** Each preview is a copy of the audio so far, so ElevenLabs
  reports the first after 64 KiB and each later one at twice the audio of the one before, at most
  ten: together at most twice the reply, which is itself capped as before.
- **The result is unchanged.** The streamed reply is gathered and read by the same code as the
  one sent at once, so the outputs and billed characters are byte-identical. Only speech streams.
- **No redirect is ever followed.** `Http::send_streaming_no_redirect` is the streaming twin of
  `send_no_redirect`; its default waits for the whole reply through `send_no_redirect`, so a
  transport without streaming still never replays a paid request elsewhere.
- **Not shown yet.** No card can play a growing MP3 (playback reads imported assets through
  ffmpeg), so the engine leaves previews off and ignores the `partial` of a progress report. Playing
  them is a follow-up for the audio card.
