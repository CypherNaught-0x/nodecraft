## 2026-10-24 · Azure OpenAI reuses the v1 chat dialect (#45)

- **Reuse the existing LLM adapter.** Microsoft's [v1 API](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle?tabs=key)
  accepts OpenAI clients with a resource-specific `/openai/v1` base and API key. A validated
  `Endpoint::azure_openai` preset and the existing `llm` feature suffice; there is no Azure SDK,
  new wire dialect or replacement of the LLM layer discussed in #489.
- **Deployment identity stays explicit.** The shipped text card has a `deployment` parameter
  (1–64 ASCII letters, digits, hyphens, underscores or dots), mapped to the request's `model`
  only for Azure's reserved `nodecraft-deployment` catalog entry. Other host catalogs'
  `provider_model` and direct `ChatRequest.model` carry deployment names unchanged. The provider id and keychain
  account are `azure-openai`; `ProviderSettings.azure_openai_url` contains only the address.
  Azure v1 gets `max_completion_tokens`, like OpenAI, so reasoning deployments honor the cap.
- **No invented rate.** Deployments and prices vary by Azure account and region. The generic
  text-only catalog card uses `ByUsage`, requiring existing explicit unknown-price admission.
  Token usage is retained, but no dollar charge can be settled without rates or a provider cost.
  Custom hosts may supply a validated catalog with actual rates; dynamic catalog integration
  remains #489. The [provider guide](../providers.md) states the limitations and native/browser
  setup boundaries. Generic quality/latency values are only discovery hints.
- **Shared lifecycle and constraints.** Chat finishes at submit; stream parsing, usage,
  content filtering, error kinds and size limits reuse `LlmClient`. There is no new remote
  polling/cancellation API or idempotency promise. Azure disables automatic retry of uncertain
  submissions, proved by an actual runner/driver lost-response regression. API-key auth uses a secret header;
  Entra authentication, dated deployment APIs and Responses are outside this preset.
- **Evidence is offline.** Fixture contracts check request routing and deployment identity,
  completion caps, streaming usage, synchronous lifecycle and errors. A gated live smoke
  test is supplied but no paid Azure call was made. The real model picker is captured before
  and after adding the catalog row; no bespoke UI is introduced.
