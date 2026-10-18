# Providers

Model services NodeCraft talks to, how to set them up, and what they can do. Each provider is a
module in `crates/providers` behind a Cargo feature (CONTRIBUTING.md, "Adding a model or
provider"). API keys live in the OS keychain (#80), never in project files or logs.

| Provider | Feature | Auth | Modes | Notes and limits |
|---|---|---|---|---|
| **OpenAI** (Chat Completions) | `llm` | API key (`Authorization: Bearer`) | text → text, image + text → text, streaming | No idempotency keys on chat: a retried prompt may be billed again. |
| **OpenAI Images** | `openai-images` | supplied OpenAI API key (`Authorization: Bearer`) | generate, ordered references/edit, first-image alpha-mask inpaint, sizes/quality/transparency/N | Two dated GPT Image 2.5 models; usage price unknown; no ambiguous automatic submit replay, remote cancellation or builtin browser transport. See below. |
| **Replicate** | `replicate` | supplied bearer token | pinned SDXL generation, img2img, mask inpaint; pinned scalar text | Versioned resumable predictions, bounded authenticated downloads; unknown usage price. See below. |
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

The evidence-aware submission path separates request delivery from whole-operation
nonacceptance. Native reqwest request building can prove a request was not executed; once
`execute` is invoked, every later error is unknown, including connection failures and redirects.
Responses, status errors, parsing, body limits and stream sink failures cannot undo possible
delivery. Browser fetch stages remain unknown.

Only the audited LLM Provider path can currently certify a whole submission: local validation
or a native pre-execute build failure, with no earlier possible delivery or outstanding work.
Legacy Provider and Http implementations default to unknown before their future constructors
are called, for **all** error kinds. Comfy remains conservative because submission may query
history/queue/catalogs, upload inputs and dispatch a prompt. A generic Auth, RateLimited or
UnsupportedInput error is not proof of nonbilling; provider-specific refusal attestations are
not implemented. This intentionally keeps more legacy failures charged as unresolved.

Scopes are invocation-local, monotonic and closed once finalized. Unknown or dropped child
work cannot certify another request, and a later local failure cannot erase an earlier ambiguous
reservation. There are no public positive proof constructors. These are audited adapter
contracts, not a sandbox for unrelated side effects in arbitrary Rust code. Direct chat/HTTP
Result APIs and the usage log format are unchanged. No message text, HTTP status or connection
heuristic establishes nonbilling.

## Live tests

Live smoke tests are opt-in and never run in CI. Each costs a few tokens:

```sh
NODECRAFT_LIVE_OPENAI=1 OPENAI_API_KEY=… cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_ANTHROPIC=1 ANTHROPIC_API_KEY=… cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_OLLAMA=1 cargo test -p nodecraft-providers --test live_llm
NODECRAFT_LIVE_COMFYUI=1 cargo test -p nodecraft-providers --test live_comfyui   # reads /object_info only
```

## OpenAI Images

`openai_images::OpenAiImages` implements the normal Provider interface behind `openai-images`.
Supply a validated Secret and an Http transport; the adapter never reads a keychain, environment,
project credential or file path. Production requests go only to api.openai.com. The native
Reqwest transport opts into send_no_redirect: no redirect, including one to the same origin,
may forward reference images. Other Http implementations must explicitly implement that operation;
its default refuses before invoking legacy send. Builtin browser execution refuses before fetch;
the library feature still compiles for wasm. The adapter does not wire a desktop paid-run host.

Official [image generation](https://developers.openai.com/api/docs/guides/image-generation)
and [API reference](https://developers.openai.com/api/reference/resources/images) were checked on
2026-10-18. The supported wire IDs are gpt-image-2.5-sunburst-2026-09-08 and
gpt-image-2.5-flare-2026-09-08, embedded as two separate catalog entries. Account/model access or
organization verification may still be required. No floating fallback model is selected.

Pass one prompt Text input, ordered image/reference Media inputs (at most 16), and optionally one
mask. No references means generation; references mean one multipart edit request. A mask applies
to the first reference; both must be PNG with matching dimensions and the mask must have alpha.
Other references accept verified PNG/JPEG/WebP. Masking is guidance rather than pixel-exact editing.
Parameters are size (1024x1024,1536x1024,1024x1536,auto), quality
(low,medium,high,xhigh,max,auto), background (opaque,transparent,auto), n (integer 1–10),
output_format (png,jpeg,webp), and output_compression (integer 0–100, JPEG/WebP only).
Transparency needs PNG/WebP. Unknown options and invalid combinations refuse before sending.
Inputs are sniffed and decoded within bounds; MIME labels alone are not validation.

One POST requests N outputs. A nonempty partial result is retained in order with its actual count;
there is no retry to fill missing images. Outputs are verified bytes, never automatically downloaded
provider URLs. Empty/excess or late-corrupt replies refuse the whole batch, potentially after billing.
Hard limits: prompt 32,000 Unicode scalars / 128,000 UTF-8 bytes; request metadata 256 KiB; input 8 MiB each,
48 MiB aggregate; complete request 64 MiB; response 65 MiB; decoded output 8 MiB each/48 MiB aggregate;
image edges 4096, pixels 16,777,216 each / 32 million aggregate; codec allowance 128 MiB per image operation.
One image is decoded at a time. Limits may only be lowered. N=10 supports ten small images; it does
not guarantee ten near-limit images fit. JSON admission bounds depth/fields/decoded strings before
owned unescaping, then strict base64 sizes are checked across the batch before payload allocations.
These bound operation-owned extents, not allocator RSS, host concurrency or network/codec internals.

Composite pricing uses ByUsage: quote is unknown, not free. Every finite cap (including zero) refuses
an unknown paid registration Some(JobPrice { estimate: None, .. }) before submit. Raw price=None is
an existing trusted-host local/free bypass; neither Provider nor ProviderRunner enforces command
partial-price consent. The run.quote/run.start authorized host path owns that consent. No numeric
pre-submit/capped desktop execution is supplied here. Optional token totals remain counts;
Usage.cost stays None rather than labelling a standard-rate estimate as an actual provider invoice.
Without an independently supplied estimate or exact monetary cost, current main may produce no
money ledger row. Do not interpret that absence as zero spend.

There is no documented server idempotency guarantee in this adapter. A lost reply can have been
billed. The captured provider's retry_ambiguous_submit=false policy makes uncertain no-handle
failures terminal in the engine, with their original diagnostic and conservative reservation handling.
Legacy providers keep their existing retry policy; genuine handles and affirmative no-send failures
retain normal resume/retry semantics. Direct clients, manual requeue/new requests, concurrent callers
and cold reconstruction can still submit again: a matching local key is not remote deduplication.
Hosts must disclose prior uncertainty before an explicit new paid attempt. Dropping a pending future
stops local waiting only; poll/cancel cannot invent a resumable handle or confirm remote cancellation.
A custom Http is a trusted Rust implementation, not a sandbox or a positive nonbilling proof.

Synthetic official-shape fixtures and authored tiny images cover all modes, options, resource/error
boundaries, native redirect refusal and actual runner composition. The live smoke test is ignored,
never runs in CI, and requires fresh human spending authorization plus both
NODECRAFT_LIVE_OPENAI_IMAGES=1 and NODECRAFT_LIVE_OPENAI_IMAGES_ACKNOWLEDGE_COST=1 before it
reads OPENAI_API_KEY. Explicit --ignored selection is also required. No paid call was used to
validate this adapter. Ordinary cargo xtask ci enables the feature for tests/Clippy/wasm.

## Replicate predictions

The optional `replicate` adapter uses the public predictions API and two catalog-pinned model
versions, verified against their public schemas on 2026-10-18. SDXL supports 512/768/1024 dimensions,
one to four images, optional negative prompt and seed, and up to 100 inference steps. Its mask is a
PNG matching the upright input image: black preserves and white repaints. The small hello-world
model returns scalar text; text resembling a URL remains text. Runtime hints are provider-reported,
not measurements or price estimates. A pinned version may be retired or upgraded by the service;
returned model/version mismatches are refused rather than silently accepted.

Inputs use local data URIs only: PNG/JPEG/WebP file bytes before base64 are capped at 256,000 bytes each and 512,000 aggregate. The complete encoded JSON request has a separate 4 MiB cap. Every image header,
claimed MIME and aggregate geometry is checked before sequential codec validation. Images are capped
at 4096 per edge, 16,777,216 pixels each and 32,000,000 aggregate; codec allocation allowance is
128 MiB. The complete encoded request is admitted before URI construction. API replies are capped at
2 MiB, 64 object fields, 256 values and depth 16. A borrowed lexical pass separately budgets the
ignored root input echo, complete output, 256 KiB of other encoded string metadata, and 64 KiB of
framing/scalars/whitespace. Root input/output regions are exclusive; duplicate root regions are refused.
Before decoding, encoding or submitting, a conservative count reserves the input echo together with the
maximum admitted output and the other response regions. It allows ASCII-escaped Unicode and escaped
slashes, charges numeric spellings up to 64 bytes, and reserves 2,048 bytes for known absent SDXL
defaults/finite normalization. The 19-field pinned schema's absent defaults require less than 496 bytes;
this reserve does not promise unlimited future fields or arbitrary expanded equivalent JSON. A queued
null output remains possible even under lowered text limits. Impossible simultaneous maxima (such as
two control-heavy maximum text roles plus image and mask) are refused before a paid submit. Individual
input maxima therefore remain subject to this aggregate echo budget as well as the 4 MiB request cap.
Requested output cardinality is checked before typed deserialization/unescaping; bounded output arrays
then use ordinary serde parsing, not a custom visitor. No owned echoed-input tree is constructed.
Unknown echoed inputs/logs are ignored within their separate finite bounds.

A versioned handle captures the pinned model, output shape and requested count. Even an immediately
finished prediction first publishes this handle. Polling and best-effort cancellation use only its
validated prediction ID. Lost poll/download replies can resume that prediction; ambiguous submission
without a handle is never automatically replayed. Explicit manual submission or a cold restart without
a retained handle can still incur a second charge. Cancellation is not proof that execution or billing
stopped. Partial nonempty success up to the requested count is accepted; expired outputs are reported
without automatic paid regeneration.

All output URLs are admitted before downloads: HTTPS on replicate.delivery or its subdomains, no
credentials/fragment/nonstandard port. Bearer-authenticated API and file requests never follow
redirects. Each file is capped at 16 MiB and its actual HTTP response cap is reduced by the remaining
48 MiB aggregate allowance. PNG/JPEG/WebP bytes are decoded and validated before publishing the whole
Completed result. No partial result escapes after a late corrupt file. The lifetime budget includes
API bytes, retained output bytes, a bounded Vec-to-Arc copy and codec allowance; this is not an RSS bound.

Pricing is `ByUsage` USD with no fabricated estimate or actual cost. A paid registration with an unknown
estimate is refused under every finite budget cap, including zero. An explicitly uncapped host may
proceed after its own partial-price consent; absent monetary evidence can produce no money ledger row,
not a claim of free execution. `price=None` is reserved for trusted free/local host registration.
The library does not implement a desktop paid enqueue host. Native no-redirect HTTP is supported;
builtin browser HTTP refuses before fetch. Wasm compilation does not demonstrate browser networking.
The ignored live smoke test requires both explicit environment opt-ins and fresh human spending
permission; ordinary tests use synthetic fixtures and local listeners only.
