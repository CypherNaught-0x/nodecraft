## 2026-10-24 · OpenAI speech and file transcription (#39)

- **Use the current file APIs, with an explicit speech expiry.** `openai-audio` is a separate
  optional provider with the existing OpenAI key. Catalog entries select the dated
  `gpt-4o-mini-tts-2025-12-15` speech model and current `gpt-transcribe`. OpenAI's
  [deprecation notice](https://developers.openai.com/api/docs/deprecations) says the speech
  endpoint models shut down on 2027-01-06. The speech picker label and description show this
  date; migration is required before then. Realtime is a different transport and not supported
  by this adapter. A replacement model cannot be substituted into `/audio/speech`.
- **Share execution, not a chat payload.** Both operations are synchronous provider submits
  behind the existing Generate audio/text cards, catalog schemas, routing, asset store and
  RunHost. Speech returns MP3; transcription uploads one bounded recording as multipart and
  returns text. The initial supported inputs are MP3, WAV, FLAC, Ogg and M4A, up to 25 MB.
  No streaming, translation, diarization, custom voices or remote cancellation is claimed.
- **Unknown cost is not zero.** Both catalog prices are `ByUsage`, requiring explicit partial
  quote acceptance. Speech has no reported token usage here. Transcription records the
  published $0.0045/minute rate only when the server reports a valid duration; missing or
  unrecognized usage stays unknown. Input metadata is not evidence of billed duration.
- **Do not replay uncertain paid requests.** The provider disables ambiguous automatic
  submits, uses the no-redirect HTTP boundary and arms delivery evidence before returning its
  future. Validation finishes before the paid boundary. A lost reply remains possibly billed,
  including when the scheduler's general retry policy permits three attempts. Errors omit
  provider details, prompts and keys. Unit and host tests use synthetic responses; the paid
  two-call live smoke is ignored and requires both opt-in guards.
