## 2026-11-08 · Bedrock's native Converse API is a third dialect over our own Http (#1150)

Follows [the rig-core decision](2026-11-06-llm-layer-rig.md) and
[the Bedrock OpenAI route](2026-11-03-bedrock-openai-compatible-route.md).

- **Not through a crate.** `rig-core` 0.46 has no Bedrock provider, and `rig-bedrock` pulls in the
  AWS SDK (tokio, hyper, a TLS stack) that doesn't build for wasm. `Dialect::Converse`
  (`crates/providers/src/llm/converse.rs`) builds the Converse JSON and reads the reply itself,
  over `nodecraft_providers::Http`, with the same rules as the other dialects: validation before
  sending, the shared reply caps and timeouts, keys as secret headers, a submit through the
  runner's delivery scope, and no automatic resend of a request that may have been sent.
- **Auth is a Bedrock API key as `Authorization: Bearer`.** AWS documents it for Converse,
  with a `curl` example to `/model/{modelId}/converse`
  ([API keys](https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys-use.html), checked
  2026-11-08). **SigV4 is left for a follow-up**: a signer alone is small, but it needs an AWS
  credential source NodeCraft doesn't have (an access key, secret and session token in the
  keychain, their expiry, settings and readiness), which is its own design.
- **The event stream is parsed here, in safe Rust.** ConverseStream answers in AWS event-stream
  framing, not SSE. `converse/event_stream.rs` checks the prelude (and its CRC32) before
  believing a length, caps a message at 1 MiB and its headers at 128 KiB, verifies the message
  CRC32, and reads headers with bounds checks; a stream cut mid-message is a `Network` error,
  anything corrupt is `Malformed`. The CRC32 is a compile-time table, no dependency.
- **A separate endpoint, not a replacement.** `Endpoint::bedrock_converse` shares the `bedrock`
  id, keychain account and Region rules; `Endpoint::bedrock` stays the OpenAI-compatible route the
  GPT OSS cards use, since switching them needs each card re-checked against Converse. No catalog
  card uses Converse yet; prices stay `by_usage` (Region-dependent), and money is settled from the
  catalog only: Converse reports tokens, never a charge.
- **Mapping.** Stop reasons `end_turn`/`stop_sequence` end the turn, `max_tokens` and
  `model_context_window_exceeded` are `MaxTokens` (the reply is cut off), `tool_use` as named, `guardrail_intervened`/`content_filtered` are `BlockedContent`, the two `malformed_*`
  are `Malformed`, anything else is `Other`. Errors by name (`x-amzn-ErrorType`, `__type`, or the
  stream's `:exception-type`): throttling is `RateLimited` (with `Retry-After` when sent), a
  used-up service quota `Budget` (not transient; the message says to raise the quota), access and signature errors `Auth`, validation `UnsupportedInput` (context-window
  messages as the context-limit error), model-not-ready and service-unavailable `Busy`, model
  timeout `Timeout`, not found `NotFound`, internal/model errors `Server`. Reasoning blocks come
  back as `Part::Opaque` and go back unchanged in later turns. A tool result's `status` goes only
  to models AWS documents it for (Amazon Nova, Anthropic Claude: ids containing `amazon.nova` or
  `anthropic.`); other models get a failed result's text prefixed with `Error: `.
