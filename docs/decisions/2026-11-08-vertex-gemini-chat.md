## 2026-11-08 · Vertex AI chat speaks rig's Gemini wire at our own URL (#1151)

Follows [the rig-core decision](2026-11-06-llm-layer-rig.md) and
[Gemini images on Google Cloud](2026-10-23-vertex-gemini-images.md).

- **Through the crate, not a native dialect.** `rig-core` 0.46 has no Vertex AI provider, but its
  Gemini GenerateContent wire builds the same REST body Vertex AI's `v1` takes and reads the same
  reply. Checked 2026-11-08 against Google's references:
  [generateContent](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rest/v1/projects.locations.publishers.models/generateContent)
  (`contents`, `systemInstruction`, `tools`, `toolConfig`, `safetySettings`, `generationConfig`),
  [streamGenerateContent](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rest/v1/projects.locations.publishers.models/streamGenerateContent)
  (a stream of the same reply), the
  [reply](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rest/v1/GenerateContentResponse)
  (`candidates[].content.parts`, `finishReason`, `promptFeedback.blockReason`, `usageMetadata`
  with `thoughtsTokenCount` counted apart from `candidatesTokenCount`),
  [FunctionDeclaration](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rest/v1/projects.locations.cachedContents)
  (`parametersJsonSchema`, exclusive with `parameters`), the v1
  [FunctionCall](https://docs.cloud.google.com/ruby/docs/reference/google-cloud-ai_platform-v1/latest/Google-Cloud-AIPlatform-V1-FunctionCall)
  and [FunctionResponse](https://docs.cloud.google.com/ruby/docs/reference/google-cloud-ai_platform-v1/latest/Google-Cloud-AIPlatform-V1-FunctionResponse)
  messages (no `id`; the response's result under `output` or `error`), and
  [thought signatures](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/thought-signatures).
  The differences are small body fixes, so `LlmClient::google_vertex` sends through the crate
  (`llm/vertex.rs`), as OpenAI and Anthropic do:
  - members the wire writes as `null` are left out, and the system instruction names no role;
  - call and response `id`s are removed (`v1` matches responses by name and order, so results are
    sent in the order of the calls), and a result goes under `output`;
  - a call's thought signature is kept with the reply (as a marker part) and put back on that call
    in the next turn; in a signed turn the wire's `skip_thought_signature_validator` placeholder
    is removed from the unsigned parallel calls, and a turn from another model keeps it.
- **Our URL and our token.** Requests go to
  `https://{location-}aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:generateContent`,
  or `:streamGenerateContent?alt=sse` (`alt=sse` is Google's
  [system parameter](https://docs.cloud.google.com/apis/docs/system-parameters) for a JSON stream as
  server-sent events). The crate gets an empty key and its URL (which would carry it as `?key=`) is
  replaced. The access token comes from the existing service-account `Tokens` (an hour's token,
  replaced five minutes before it lapses, exchanged free, dropped on a 401) and goes on the
  transport as a secret `Authorization` header; it never passes through the crate, and an error
  that quotes it has it removed. A refused token exchange sends nothing to Vertex AI.
- **Guards on what the crate reads.** The crate's Gemini decoder sizes nothing by a wire index
  (its blocks are numbered by the decoder), but it keeps candidates by their `index` with a linear
  lookup and matches every grounding segment against the whole answer on every event. Streams are
  checked first: at most 8 candidates with indices under 8, no grounding segments (NodeCraft never
  asks for grounding), at most 64 citations an event, and at most the reply block limit of parts
  that open a block. Whole replies have grounding and citation metadata removed before the crate
  reads them.
- **Mappings.** `STOP` ends the turn (a tool use when it holds calls), `MAX_TOKENS` is a length
  stop, `SAFETY`, `RECITATION`, `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `MODEL_ARMOR` and the
  image filters are `BlockedContent`, as is a blocked prompt; other reasons (such as
  `MALFORMED_FUNCTION_CALL`) are kept by name. Output tokens are `candidatesTokenCount` plus
  `thoughtsTokenCount`, since thinking is billed as output; input adds `toolUsePromptTokenCount`.
  Errors are read from Google's canonical status: `UNAUTHENTICATED` and `PERMISSION_DENIED` are
  `Auth` (with `BILLING_DISABLED` as `Budget`), `RESOURCE_EXHAUSTED` is `RateLimited` unless it
  names a daily quota (`Budget`), `INVALID_ARGUMENT` is `UnsupportedInput` (a token-limit message
  is the context-limit error), `NOT_FOUND`, `UNAVAILABLE` and `DEADLINE_EXCEEDED` as named.
- **Express mode** API keys are supported in the library only
  (`LlmClient::google_vertex_express`, `x-goog-api-key` on
  `aiplatform.googleapis.com/v1/publishers/google/models/…`), since it is the same dialect with
  another header. Google offers it to new accounts for a 90-day free tier, a listed set of models
  and small quotas
  ([express mode](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/start/express-mode/overview),
  checked 2026-11-08), so no card or keychain account uses it yet.
- **Money.** One card, `google-gemini-llm.3.8-flash`, on the global location with the
  `google-cloud` service account. It is `by_usage`: Google's
  [pricing page](https://cloud.google.com/vertex-ai/generative-ai/pricing), read 2026-11-08, lists
  an introductory $0.75 / $3.75 per million input / output tokens through 2026-12-31 and $1.50 /
  $7.50 from 2027-01-01, a change on a date the catalog can't express, and thinking has no bound
  before the run. No automatic resend of a request that may have been sent, as for every chat.
- **Not done.** The live smoke test (`live_google_vertex1151`) has not been run. Locations other
  than `global` are supported by the client but not offered in settings.
