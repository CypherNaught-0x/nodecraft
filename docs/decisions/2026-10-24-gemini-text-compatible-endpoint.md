## 2026-10-24 · Direct Gemini text with the existing chat client (#44)

- **Reuse Google's compatibility API.** The Gemini text card sends directly to Google's
  documented `generativelanguage.googleapis.com/v1beta/openai/chat/completions` endpoint,
  using the existing OpenAI-shaped `LlmClient`. It does not require an aggregator. This is
  the adapter reuse explicitly allowed by #44; it adds no `generateContent` or Interactions
  dialect, client-library dependency, or architecture decision for the still-open #489.
- **A bounded text-only surface.** One stable Gemini 3.8 Flash card accepts a prompt and
  earlier text turns. It caps total text at 1 MiB, inputs at 64 and requested output at
  65,536 tokens. The default is 4,096. Media, tools, grounding, remote conversation storage
  and Gemini-specific thinking controls are not exposed. The shared parser retains its
  response and stream bounds; the host can stream text through its normal delivery context.
- **Shared key, conservative money semantics.** Native hosts read the existing `gemini`
  keychain account; the key is a secret Authorization header. No idempotency guarantee is
  assumed: an ambiguous paid submit is terminal and remains available for reconciliation.
  Synchronous requests have no poll handle and cannot confirm remote cancellation.
  Pricing is explicitly usage based. Token counts are retained, but no dollar charge is
  invented from account tiers or assumptions about thinking-token reporting. The ordinary
  planner therefore requires explicit unknown-price acceptance.

Contract checked 2026-10-24: [Google compatibility API](https://ai.google.dev/gemini-api/docs/openai),
[stable model and limits](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash),
[account-tier pricing](https://ai.google.dev/gemini-api/docs/pricing).
The [API overview](https://ai.google.dev/gemini-api/docs) recommends Interactions for new wire
integrations; this change instead retains NodeCraft's existing supported compatibility protocol.
