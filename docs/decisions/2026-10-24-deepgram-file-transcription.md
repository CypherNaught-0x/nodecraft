## 2026-10-24 · Deepgram file transcription (#40)

- **Direct, bounded file transcription.** Nova-3 uses the documented synchronous
  `POST /v1/listen` contract and the shared provider runner. No existing builtin adapter
  implements this wire shape; no SDK or second runner is introduced.
- **A narrow initial card.** English, smart formatting, one WAV/MP3/FLAC/Ogg recording,
  at most 25 MiB and 30 minutes. Readable audio headers must report 1–2 channels and
  8–192 kHz; this is header validation, not full decoding. Stereo is transcribed together.
  URLs, callbacks, live streaming and extra analysis features are outside this card.
- **Protect credentials and bound results.** The native host reads the `deepgram` keychain
  account; fixed-origin secret authorization never follows redirects. Error messages omit
  provider text. Replies are capped at 4 MiB and transcripts at 1 MiB, including empty text
  for silence. `mip_opt_out=true` excludes requests from the model improvement program.
- **Do not invent billing evidence.** Usage pricing requires explicit unknown-price consent.
  Neither audio duration nor absent cost metadata is a dollar receipt. No ambiguous paid
  submit is retried, no handle is fabricated and cancellation cannot promise a remote stop.
  The transport timeout is ten minutes, independent of the runner's local deadline.
- **Verify without spending.** Contract fixtures, the real host/driver, catalog admission
  and pointer selection exercise the shipping path. An ignored live smoke requires two
  explicit opt-ins and uses the existing public tone fixture.

Sources: [Deepgram prerecorded contract](https://developers.deepgram.com/reference/speech-to-text/listen-pre-recorded),
[getting started](https://developers.deepgram.com/docs/pre-recorded-audio),
[opt-out](https://developers.deepgram.com/docs/the-deepgram-model-improvement-partnership-program).
