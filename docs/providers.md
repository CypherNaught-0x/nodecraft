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
| **LiteLLM** | `llm` | virtual key (bearer) | as configured on the proxy | Spend per request from `x-litellm-response-cost` (reaches budgets as the job's `Usage::cost`); a key's total spend and budget from `/key/info` (`LlmClient::litellm_key_info`); models from `/v1/models`. |
| **Anthropic** (Messages) | `llm` | API key (`x-api-key`, `anthropic-version: 2023-06-01`) | text, image + text → text, streaming | Default model `claude-opus-5-5`. Current models reject sampling parameters, so `temperature` is only sent when set; thinking is adaptive and not configured. A `refusal` stop is reported as blocked content. |
| **ComfyUI** | `comfyui` | none locally; optional auth header for servers behind a proxy | whatever the workflow does (image, video, audio, 3D) | Jobs carry an API workflow, `<node>.<input>` value overrides and media inputs (uploaded under content-hash names). Missing custom nodes and models are reported before queueing. Prompt ids are salted per install; retries never queue twice. Only `output` files are results (previews aren't kept). Progress is queue position and completion until the WebSocket lands. Servers too old for prompt-scoped `/interrupt` stop whatever runs when you cancel. |

## Chat conversations and tools

`llm::ChatRequest` is a whole conversation: user messages (text, images, tool results) and
assistant messages (text, tool calls), plus the `tools` the model may call. Both dialects take the
same request.

- **Append the reply as it is.** `ChatResponse::message` is the reply as an assistant message.
  Push it onto `messages` unchanged before the next turn. Claude's thinking blocks come back as
  `Part::Opaque` and must be sent back untouched, or the model loses its earlier reasoning. They
  are left out when the conversation goes to an OpenAI-style endpoint.
- **Tool calls** are `Part::ToolUse` in the reply (`ChatResponse::tool_calls`). Answer every call
  in the next user message with a `Part::ToolResult` carrying its id; the request is refused
  before sending otherwise. Validate a call's `input` against the tool's schema before running
  it: a server can send arguments that aren't JSON, which arrive as a string.
- **No forced calls.** Tools are always offered as `auto`, because current Claude models reject
  forcing one. Name the tool in the prompt, check the reply for a call, and set `strict` for
  schema-valid arguments. OpenAI's strict mode also needs every property in `required` and
  `additionalProperties: false`.
- **Streaming** carries tool calls and thinking too: `chat_stream` passes text to the callback
  and returns the same `ChatResponse` as `chat`.
- **Jobs** (Text nodes) carry history as job inputs in order: `system`, then the user's text and
  images, and earlier replies as `assistant` text.
- **Usage**: token counts and any reported cost are in `ChatResponse::usage`. For jobs they are in
  the `Completed` result that `run_to_completion` returns.

## Submission certainty

`RunFailure::uncertain` describes an interrupted or ambiguous submit with no returned handle.
A false value alone does not establish that a run was unbilled: a handle independently proves
provider acceptance and possible billing. Cancellation set before the engine starts reserves nothing;
cancellation while its future is pending may already have reached the provider and keeps the
estimate as an unresolved charge. Constructing a submit future itself may have side effects.

The current HTTP contract returns `ProviderError` without delivery-stage evidence. Local JSON
encoding failures and unreadable replies both use `Malformed`; connection failures and dropped
replies share `Network`. Redirect failures may follow earlier requests. Those generic kinds
cannot prove that a submission was never accepted, so ambiguous failures stay conservative.
A transport distinction would need explicit evidence that nothing was sent, preserved across
redirects and composed by adapters with any earlier requests. This requires a separate transport
contract change tracked in [#250](https://github.com/CypherNaught-0x/nodecraft/issues/250); no
message text or connection-error heuristic establishes nonbilling here.

## Live tests

Live smoke tests are opt-in and never run in CI. Each costs a few tokens:

```sh
NODECRAFT_LIVE_OPENAI=1 OPENAI_API_KEY=… cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_ANTHROPIC=1 ANTHROPIC_API_KEY=… cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_OLLAMA=1 cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_COMFYUI=1 cargo test -p nodecraft-providers --test live_comfyui   # reads /object_info only
```
