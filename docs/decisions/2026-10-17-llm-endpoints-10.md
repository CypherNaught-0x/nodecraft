# 2026-10-17 · LLM endpoints (#10)

- **Two wire dialects** cover every chat endpoint in scope: OpenAI Chat Completions (OpenAI,
  Ollama, LM Studio, vLLM, OpenRouter, LiteLLM) and Anthropic Messages. Endpoints are data
  (`llm::Endpoint` presets), not separate providers.
- **Chat finishes at submit** (`Submission::Finished`); streaming goes through
  `LlmClient::chat_stream` and `Http::send_streaming` with an incremental SSE parser capped per
  event.
- **Refusals and content filters are errors** (`BlockedContent`), never partial text presented as
  a result. Quota errors that arrive as HTTP 429 map to `Budget`, not a retryable rate limit.
- **Claude defaults:** `claude-opus-5-5`; no `thinking` parameter (current models think
  adaptively). A `temperature` sent to a model that rejects sampling (5-series and later) is an
  early, clear error rather than an API 400; Anthropic's range is 0–1.
- **Reply limits:** OpenAI itself gets `max_completion_tokens` (its reasoning models reject
  `max_tokens`); other OpenAI-compatible servers get a limit only when the job sets one, since
  small local models reject large defaults. Anthropic always gets one (required).
- **Streams are capped by reply length** (~400 bytes per token of `max_tokens`, at least 8 MiB, at
  most 128 MiB) with a 60-minute timeout, so a long reply isn't cut off after it was billed.
- **Deferred to #195:** multi-turn history, tool calling, and passing usage/cost into budgets.
