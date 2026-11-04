## 2026-11-03 · Amazon Bedrock reuses the OpenAI chat dialect with an API key (#46)

- **A preset, not a new wire.** AWS documents an OpenAI-compatible Chat Completions route on
  `bedrock-runtime` at `https://bedrock-runtime.{region}.amazonaws.com/openai/v1/chat/completions`,
  authenticated with a Bedrock API key as `Authorization: Bearer`
  ([Chat Completions API](https://docs.aws.amazon.com/bedrock/latest/userguide/inference-chat-completions.html),
  [API keys](https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys-use.html), checked
  2026-11-03). Like Azure ([2026-10-24](2026-10-24-azure-openai-v1.md)), `Endpoint::bedrock` puts
  the existing `Dialect::OpenAi` at a regional base. No AWS SDK, no new dialect, and nothing
  that pre-empts the #489 decision.
- **SigV4 and Converse are deferred to #489.** SigV4 needs an AWS credential source and the
  native Converse API is a third wire; both wait for that record. The key comes only from the
  keychain account `bedrock`: no `AWS_BEARER_TOKEN_BEDROCK`, profiles, access keys or instance
  metadata. The non-secret `ProviderSettings.bedrock_region` holds a validated Region code, not a
  URL, so the key's host is always `bedrock-runtime.<region>.amazonaws.com`; partitions on
  other domains (China `cn-`, European Sovereign Cloud `eusc-`, isolated `us-iso-`/`us-isob-`)
  are refused. `bedrock-mantle` (a compatibility endpoint with per-model path prefixes) is not used.
- **Two documented models, no invented rate.** The catalog ships the model cards' exact
  `bedrock-runtime` ids for gpt-oss-120b and gpt-oss-20b, which list Chat Completions there, text
  only, 16K output. AWS's [pricing page](https://aws.amazon.com/bedrock/pricing/) publishes their
  rates per service tier and for one Region (Asia Pacific (Sydney)) only, with no rate for the
  other Regions the cards list, so both use `ByUsage` with explicit unknown-price admission. The
  cap goes out as `max_completion_tokens`, the field the OpenAI-compatible route's examples use
  ([OpenAI models](https://docs.aws.amazon.com/bedrock/latest/userguide/model-parameters-openai.html));
  the model cards' InvokeModel samples use `max_tokens`, which this route does not send.
- **Shared lifecycle; no automatic paid replay.** Chat finishes at submit; streaming, usage, error
  kinds and size limits reuse `LlmClient`. AWS documents no idempotency for this route, so
  `retry_ambiguous_submit` is false, as for Azure, with a runner regression proving it.
- **No `keys.test` probe.** `bedrock-runtime` does not implement the OpenAI model list, and the
  other candidate routes need a Region the check does not have; `bedrock` answers unverifiable.
- **Evidence is offline.** Fixture contracts and a gated live smoke test
  (`NODECRAFT_LIVE_BEDROCK=1`) that never runs in CI; no paid Bedrock call was made.
