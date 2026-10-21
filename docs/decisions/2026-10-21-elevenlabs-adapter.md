## 2026-10-21 · ElevenLabs is six catalog models, speech is priced by its characters (#20)

- **One catalog model per ElevenLabs model or endpoint**: three speech models (v3, Multilingual v2,
  Flash v2.5), sound effects, music and the voice changer. Each has its own price rule and text
  cap, which the catalog can't vary by parameter.
- **Speech is priced per 1,000 characters with no count**, so its quote is unknown until the run,
  as for token-priced chat; the result records the characters sent as billable units. Quoting
  from the prompt's length is a follow-up.
- **Sound effects are estimated at 0.12 USD per call.** The pricing page lists the same figure
  per minute and per generation; per generation is the higher for clips under a minute.
- **The voice changer is by usage**, since its price follows the recording's length.
- **Voices are a fixed list of premade ids plus a typed override** that must be ASCII letters and
  digits, so it can't reshape the request path. Loading the account's voices is a follow-up.
- **Only documented refusals release the reservation**: 401 `invalid_api_key`/`quota_exceeded`,
  a 422 validation list, and 400/429 with `quota_exceeded`, `voice_not_found`,
  `too_many_concurrent_requests`, `system_busy` or `max_character_limit_exceeded`. Anything else
  after the request is sent stays possibly billed, and a submit is never retried automatically.
