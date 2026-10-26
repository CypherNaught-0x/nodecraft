## 2026-10-24 · Hugging Face text through the existing chat client (#48)

- **Reuse the documented compatibility route.** A bounded wrapper uses the existing
  `LlmClient` with `https://router.huggingface.co/v1`. This is the reuse allowed by #48;
  it adds no SDK, new dialect or client-library choice for the separate #489 decision.
- **Select an explicit provider.** The single text card uses `openai/gpt-oss-120b:groq`.
  The official router model listing reported the Groq route live on this date. Keeping the
  suffix avoids automatic fastest/cheapest selection changing the selected provider.
- **Keep a narrow contract.** Text only, 64 turns, 1 MiB text, 256-byte request keys and
  1–8,192 requested completion tokens (default 2,048). Existing response/stream limits apply.
  Media, tools and other inference tasks are not represented as supported modes.
- **Conservative billing and credentials.** The native host reads the `huggingface` keychain
  account. Bearer credentials are marked secret. Unknown dollar usage requires explicit
  acceptance; token counts are evidence but never invented dollar charges. No ambiguous
  paid submit is replayed. Synchronous calls have no poll handle, and cancellation cannot
  confirm a remote stop. Contract tests include payment-required and rate-limit mappings,
  real host routing, admission and the actual lost-reply driver path. Live tests are opt-in.

Sources checked 2026-10-24: [router and explicit provider routing](https://huggingface.co/docs/inference-providers/en/index),
[chat contract](https://huggingface.co/docs/inference-providers/tasks/chat-completion),
[model availability](https://router.huggingface.co/v1/models),
[pricing and credits](https://huggingface.co/docs/inference-providers/pricing).
