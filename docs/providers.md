# Providers

Model services NodeCraft talks to, how to set them up, and what they can do. Each provider is a
module in `crates/providers` behind a Cargo feature (CONTRIBUTING.md, "Adding a model or
provider"). API keys live in the OS keychain (#80), never in project files or logs.

| Provider | Feature | Auth | Modes | Notes and limits |
|---|---|---|---|---|
| **OpenAI** (Chat Completions) | `llm` | API key (`Authorization: Bearer`) | text → text, image + text → text, streaming | No idempotency keys on chat: a retried prompt may be billed again. |
| **Ollama** | `llm` | none (local, `http://127.0.0.1:11434`) | text, vision models, streaming | OpenAI-compatible `/v1`; models listed from `/v1/models`. Pull a model first. |
| **LM Studio** | `llm` | none (local, `http://127.0.0.1:1234`) | text, vision models, streaming | OpenAI-compatible server. |
| **vLLM / other gateways** | `llm` | optional bearer key | text, streaming | Any OpenAI-compatible base URL (`Endpoint::openai_compatible`). |
| **OpenRouter** | `llm` | API key | text, vision, streaming | Model ids like `anthropic/claude-opus-5-5`. |
| **LiteLLM** | `llm` | virtual key (bearer) | as configured on the proxy | Spend per request from `x-litellm-response-cost`; models from `/v1/models`. |
| **Anthropic** (Messages) | `llm` | API key (`x-api-key`, `anthropic-version: 2023-06-01`) | text, image + text → text, streaming | Default model `claude-opus-5-5`. Current models reject sampling parameters, so `temperature` is only sent when set; thinking is adaptive and not configured. A `refusal` stop is reported as blocked content. |
| **ComfyUI** | `comfyui` | none locally; optional auth header for servers behind a proxy | whatever the workflow does (image, video, audio, 3D) | Jobs carry an API workflow, `<node>.<input>` value overrides and media inputs (uploaded under content-hash names). Missing custom nodes and models are reported before queueing. Prompt ids are salted per install; retries never queue twice. Only `output` files are results (previews aren't kept). Progress is queue position and completion until the WebSocket lands. Servers too old for prompt-scoped `/interrupt` stop whatever runs when you cancel. |

## Live tests

Live smoke tests are opt-in and never run in CI. Each costs a few tokens:

```sh
NODECRAFT_LIVE_OPENAI=1 OPENAI_API_KEY=… cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_ANTHROPIC=1 ANTHROPIC_API_KEY=… cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_OLLAMA=1 cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_COMFYUI=1 cargo test -p nodecraft-providers --test live_comfyui   # reads /object_info only
```
