# Providers

Model services NodeCraft talks to, how to set them up, and what they can do. Each provider is a
module in `crates/providers` behind a Cargo feature (CONTRIBUTING.md, "Adding a model or
provider"). API keys live in the OS keychain (#80), never in project files or logs.

| Provider | Feature | Auth | Modes | Notes and limits |
|---|---|---|---|---|
| **OpenAI** (Chat Completions) | `llm` | API key (`Authorization: Bearer`) | text → text, image + text → text, streaming | No idempotency keys on chat: a retried prompt may be billed again. Builtin catalog (`assets/catalog/openai.toml`, #503): GPT-6.1 Sol, GPT-6 Luna, GPT-4.1 mini, text → text, quoted at the output price on `max_tokens` (input billed on top; GPT-6.1 Sol and GPT-6 Luna at their 1.5x rate for prompts over 272K tokens, so the quote stays an upper bound); only GPT-4.1 mini offers `temperature`. |
| **OpenAI Images** | `openai-images` | supplied OpenAI API key (`Authorization: Bearer`) | generate, ordered references/edit, first-image alpha-mask inpaint, sizes/quality/transparency/N | Two dated GPT Image 2.5 models; usage price unknown; no ambiguous automatic submit replay, remote cancellation or builtin browser transport. See below. |
| **Gemini Images** | `gemini-images` | supplied Gemini API key (`x-goog-api-key`) | generation and direct-adapter reference editing | Three stable Gemini models, unknown usage price; canvas resolver/mixed/list routing limitations below. |
| **Replicate** | `replicate` | supplied bearer token | pinned SDXL generation, img2img, mask inpaint; pinned scalar text | Versioned resumable predictions, bounded credential-free delivery downloads; unknown usage price. See below. |
| **Ollama** | `llm` | none (local, `http://127.0.0.1:11434`) | text, vision models, streaming | OpenAI-compatible `/v1`; models listed from `/v1/models`. Pull a model first. |
| **LM Studio** | `llm` | none (local, `http://127.0.0.1:1234`) | text, vision models, streaming | OpenAI-compatible server. |
| **vLLM / other gateways** | `llm` | optional bearer key | text, streaming | Any OpenAI-compatible base URL (`Endpoint::openai_compatible`). |
| **OpenRouter** | `llm` | API key | text, vision, streaming | Model ids like `anthropic/claude-opus-5-5`. |
| **LiteLLM** | `llm` | virtual key (bearer) | as configured on the proxy | Spend per request from `x-litellm-response-cost` (reaches budgets as the job's `Usage::cost`); a key's total spend and budget from `/key/info` (`LlmClient::litellm_key_info`); models from `/v1/models`. |
| **Anthropic** (Messages) | `llm` | API key (`x-api-key`, `anthropic-version: 2023-06-01`) | text, image + text → text, streaming | Default model `claude-opus-5-5`. Builtin catalog (`assets/catalog/anthropic.toml`, #503): Opus, Sonnet and Haiku 5.5, text → text, quoted at the output price on `max_tokens` (input billed on top). Current models reject sampling parameters, so `temperature` is only sent when set; thinking is adaptive and not configured. A `refusal` stop is reported as blocked content. |
| **ComfyUI** | `comfyui` | none locally; optional auth header for servers behind a proxy | whatever the workflow does (image, video, audio, 3D) | Jobs carry an API workflow, `<node>.<input>` value overrides and media inputs (uploaded under content-hash names). Missing custom nodes and models are reported before queueing. Prompt ids are salted per install; retries never queue twice. Only `output` files are results (previews aren't kept). Progress is queue position and completion until the WebSocket lands. Servers too old for prompt-scoped `/interrupt` stop whatever runs when you cancel. |
| **fal.ai** | `fal` | host-supplied key (`Authorization: Key`) | FLUX image, Wan video, Kokoro audio, Trellis 3D, ESRGAN upscale | Five explicit live aliases; queue polling/resume, bounded whole-output downloads and best-effort cancellation. Unknown usage quotes except fixed-512 Trellis estimate. |
| **Seedream 4.5 (BytePlus ModelArk)** | `seedream` | API key (`Authorization: Bearer`) | text → one image; direct-adapter ordered multi-reference editing | Dated `seedream-4-5-251128`; one JPEG, 2K/4K, up to 14 references. No automatic paid replay or remote cancellation; no builtin browser no-redirect transport. |
| **Tripo** | `tripo` | API key (`Authorization: Bearer`) | text → 3D, image → 3D, multiview (front/left/back/right) → 3D | One textured PBR GLB per task on a Generate 3D card; dated per-task estimates. No remote cancellation and no automatic paid replay. See below. |
| **Meshy** | `meshy` | API key (`Authorization: Bearer`) | text → 3D, image → 3D, multi-image → 3D, retexture and remesh a wired GLB | One GLB per task on a Generate 3D card; dated per-task estimates. Tasks can't be cancelled. No automatic paid replay. See below. |

## Setting up catalog providers

The engine builds an adapter for every provider the model catalog names
(`engine::provider_setup`, #409). Each paid adapter reads its key from the keychain account below;
ComfyUI reads its server from `config.toml`. An adapter that can't be built is reported with what
to do, and a node routed to it is refused before planning, so no request is sent.

| Catalog provider | Keychain account | Also needs |
|---|---|---|
| `openai` (chat) | `openai` | — |
| `anthropic` (chat) | `anthropic` | — |
| `openai-images` | `openai` | — |
| `replicate` | `replicate` | — |
| `seedream` | `seedream` | — |
| `tripo` | `tripo` | — |
| `meshy` | `meshy` | — |
| `comfyui` | `comfyui:<host>:<port>`, or `comfyui:<host>` when the address names no port (optional bearer token) | `[providers] comfyui_url = "http://127.0.0.1:8188"` and the per-install `install_salt` |

Keys are read when an adapter is first built, which can show a keychain prompt, so hosts build
adapters off the UI thread and only for the providers a run uses; each run gets a fresh routing
from the adapters already built. ComfyUI's token is optional: a keychain that can't be opened or
was refused means no token, not no ComfyUI. The app makes `install_salt` once and saves it at once;
with read-only settings none is made and ComfyUI stays off.

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

A native request-builder failure returns terminal `UnsupportedInput` with the static diagnostic
`HTTP request could not be constructed`; request URLs, header values and the underlying builder
error are excluded. This applies to ordinary and streaming requests, including a later redirect
hop. An earlier hop may already have delivered the request: a later builder failure does not
establish whole-operation nonbilling, erase that uncertainty or release its reservation.
Execution, connection and timeout errors retain their existing classifications. The audited LLM path releases a fresh
reservation immediately when its complete submission proves no delivery; earlier ambiguous
attempts remain charged even when a later attempt fails locally.

HTTP middleware must propagate delivery contexts through every relevant request, but context
forwarding alone is insufficient. A wrapper with side effects outside the inner request,
including effects when constructing its future or outstanding work, must not promote an inner
not-sent proof to proof of the whole wrapper attempt. Keep the whole operation unknown or
discard inner affirmative evidence unless the entire wrapper is audited and all relevant work
participates in its scope. Existing code has no such side-effecting wrapper. Unaudited wrappers
default to unknown; observing zero socket accepts or matching an error kind grants no positive
evidence. This is an adapter contract, not a sandbox or an automatic enforcement guarantee.
Browser delivery remains conservative.

The audited LLM Provider path can certify a whole submission after local validation or a native
pre-execute build failure, with no earlier possible delivery or outstanding work. Gemini Images
can certify pure local validation failure, but deliberately marks delivery unknown before
constructing its no-redirect HTTP future; a native builder failure there remains conservative.
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
Malformed or inconsistent optional token usage is discarded as a whole while valid image outputs
are retained. Missing detail members, invalid count types, overflow and mismatched totals leave
usage unknown, not zero. This does not bypass JSON/UTF-8, depth, field, value or metadata limits,
nor base64, MIME, alpha, count, pixel or codec validation. At most 64 JSON fields and 256 values at
depth 16 are admitted; arbitrary future response expansion is not guaranteed.
Without an independently supplied estimate or exact monetary cost, current main may produce no
money ledger row. Do not interpret that absence as zero spend.

There is no documented server idempotency guarantee in this adapter. A lost reply can have been
billed. The captured provider's retry_ambiguous_submit=false policy makes uncertain no-handle
failures terminal in the engine, with their original diagnostic and conservative reservation handling.
Legacy providers keep their existing retry policy; genuine handles and affirmative no-send failures
retain normal resume/retry semantics. Direct clients, manual requeue/new requests, concurrent callers
and cold reconstruction can still submit again: a matching local key is not remote deduplication.
Hosts must disclose prior uncertainty before an explicit new paid attempt.
HTTP 429 (rate limited) and 503 (overloaded) remain terminal for uncertain no-handle submissions.
Retry-After is diagnostic information, not proof of nonacceptance or nonbilling. General provider
retry recommendations do not establish that stronger guarantee. The 180-second timeout bounds
local waiting; high quality or larger N can exceed it, losing a possibly paid reply. It does not
cancel remote work. Builtin wasm refuses before fetch, but the legacy Provider boundary remains
conservatively uncertain; that refusal carries no whole-submit positive no-send attestation. Dropping a pending future
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
2 MiB, 64 non-input object fields plus 32 fields in the ignored root input subtree, 256 values
outside that subtree plus 256 inside it, and depth 16. The echo should be a copy of our own bounded
request and is still capped, so a long echo never crowds out the status, output and metrics it
accompanies. A borrowed lexical pass separately budgets the ignored root input echo, complete output, 256 KiB of other encoded string metadata, and 64 KiB of
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
Unknown echoed inputs/logs are ignored within their separate finite bounds. The input field allowance
covers the pinned 19-property SDXL schema up to the existing 32-parameter admission scale; it does
not promise arbitrary future schema expansion or increase the raw body/metadata/output budgets.

A versioned handle captures the pinned model, output shape and requested count. Even an immediately
finished prediction first publishes this handle. Polling and best-effort cancellation use only its
validated prediction ID. Lost poll/download replies can resume that prediction; ambiguous submission
without a handle is never automatically replayed. Explicit manual submission or a cold restart without
a retained handle can still incur a second charge. Cancellation is not proof that execution or billing
stopped. Partial nonempty success up to the requested count is accepted; expired outputs are reported
without automatic paid regeneration.

If a successful create reply fails full prediction admission, a separate complete bounded JSON
receipt can retain its unique canonical root ID and request one best-effort API cancellation.
That scanner validates UTF-8, JSON grammar/Unicode escapes, depth 16, 256-byte raw key spellings,
64-byte scalar spellings and a 128-byte decoded ASCII ID within the same 2 MiB response cap.
The ID uses fixed storage; bounded semantic decoding also recognizes escaped root ID/status keys
and status values without an owned JSON tree. Duplicate semantic IDs (even equal ones), nested-only
IDs, malformed or truncated bodies, excessive depth and missing response bytes cannot supply a
trustworthy receipt. Duplicate semantic statuses cannot confirm termination. Receipt recovery after
application field/region rejection never validates outputs; full prediction admission still refuses
escaped field names, even when the receipt permits cleanup. Only a matching
complete terminal-status acknowledgment is reported as terminal; HTTP success alone is insufficient.
The original failure diagnostic retains a safe ID and cancellation disposition for manual inspection.
It remains visible in the runner/Driver failure record until the host explicitly forgets that record;
it is not a durable typed resumable handle owner. No second create or output download is sent.
Cancellation failure, a dropped future/process or an unreceived ID can still leave remote work running.
An ID containing the configured credential is refused/redacted before handle publication or any
poll/cancel URL construction; that redacted ID cannot be recovered from the diagnostic. These guards
are not a sandbox for custom trusted Rust transports. Prior billing remains uncertain even after a
terminal cancellation status; no refund or whole-submit nonacceptance proof is inferred.

All output URLs are admitted before downloads: HTTPS on replicate.delivery or its subdomains, no
credentials/fragment/nonstandard port. Only API requests carry the bearer credential; output delivery
GETs have no Authorization header. API and delivery requests both refuse redirects, including
same-origin redirects. Inaccessible/expired links and refused output redirects have download-specific
diagnostics rather than API-key or create-acceptance advice. Each file is capped at 16 MiB and its actual HTTP response cap is reduced by the remaining
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

Replicate asks for a 2-second queued/running poll interval, subject to the host's existing maximum
interval and retry/backoff policy. `Cancel-After` can bound server lifetime from prediction creation
(5 seconds to 24 hours); it is not the synchronous wait duration or a proof of nonbilling. This
adapter does not add that header: `RunLimits::deadline_ms` is a local polling deadline that is not
passed to Provider::submit, so equating those clocks would be misleading. The existing local
deadline still requests best-effort cancellation when a handle exists. Provider-reported
`metrics.predict_time` measures execution seconds, not tokens or money; current Usage has no
duration field, so it remains ignored and cost stays unknown. Neither future deadline propagation
nor a typed runtime-usage schema is supplied by this safety follow-up.


## Gemini Images (`gemini-images` feature)

The existing host provider factory reads the `gemini` keychain account for `gemini-images`,
using the same readiness and routing path as other catalog adapters. Construction sends no
requests; this adds no paid-run host or production media-input resolver.

GeminiImages uses the stateless Gemini Interactions REST API at the fixed Google origin, with store/background/stream false. Supported stable model IDs are gemini-nano-banana-2.1, gemini-3.1-flash-lite-image and gemini-3-pro-image. Vertex and retired Imagen endpoints are not implemented. The catalog offers generation and direct-adapter editing with up to fourteen ordered PNG/JPEG/WebP references. Lite is not optimized for multiple-reference or sequential editing. No masks, remote URLs, conversation state, search/tools, count, quality or output-format switches. Output is explicitly inline JPEG. Lite's wire image_size is `1K` per REST enum and guide, despite its model page's conflicting `1024px` prose. Other models offer 1K/2K/4K; the documented local ten-aspect subset is in the catalog.

Pass an API key as a Secret from the host's OS credential store, not a project field. The adapter uses x-goog-api-key as a secret header, generic User-Agent, no redirect following, and no automatic replay of uncertain paid requests. The local idempotency key is not a remote deduplication guarantee. A timeout, redirect, malformed response or cancellation after dispatch may still be billed. Poll has no resumable handle and cancel cannot confirm remote cancellation. Builtin browser transport explicitly refuses no-redirect requests; wasm compilation is supported with an audited host transport. No keys, prompts or provider response excerpts appear in adapter diagnostics.

Pricing is ByUsage USD, unknown rather than free; token counts do not become actual currency. Every finite budget cap, including zero, refuses an unknown paid registration before transport. An explicitly uncapped host may run. Missing or malformed optional usage never discards otherwise valid images; returned Usage.cost is None. Main can have no numeric ledger record when neither estimate nor actual cost is available; this provider does not repair that limitation or imply a zero bill.

The adapter preserves all final image/text blocks in order and validates the complete image collection before returning Completed. Unknown output types, URI-only output, corrupt later images and incomplete/nonterminal responses refuse rather than silently publish partial results. Thought summaries are bounded private metadata, not final output. Text-only output reports an actionable malformed/no-image result; the selected Interactions schema does not expose generateContent finishReason/safetyRatings, so arbitrary 403 or a text sentence is not guessed to be BlockedContent.

Current canvas limitations are explicit: generate::provider_job cannot resolve graph Media assets, so catalog editing data does not implement end-to-end canvas editing. Direct ProviderJob::Media editing works with supplied bytes. generate::produced needs a Text route for mixed text/images, and Run requires list ports for multiple values; scalar image ports refuse multiple images. Hosts must retain the original paid Completed while conversion/assembly may refuse, not drop text/images or retry generation. This is caller responsibility, not automatic durable result retention. Tests exercise the actual converter and Run boundaries without changing engine behavior.

Local lower-only limits: prompt 128,000 bytes / 32,000 characters; 15 inputs / 14 references; 8 MiB per input and 48 MiB total; 64 MiB request; 65 MiB response; 16 final images; 8 MiB per output and 48 MiB total. Structural limits are 64 steps / 256 content items, JSON depth 16 / 8,192 members / 16,384 values / 4,096 strings, 256 KiB metadata including 64 KiB text, and 64 MiB aggregate image base64. Images have an 8,192-pixel axis cap, 17,203,200 pixels per image and 34,406,400 pixels total. These shared image limits apply to references as well as outputs. The aggregate admits two largest documented 4K outputs, not sixteen maximum-size images; every byte, count and pixel bound still applies. Any later invalid or excessive image refuses the whole collection.

For Nano and Pro, requested 1K/2K/4K geometry is checked against lowered axis, per-image, two-image aggregate and JPEG pixel/orientation allowances before reference decoding or POST. This cannot predict the actual returned count or encoded byte length. Lite has no separately identified dimension table in the checked guide: its default envelope is supported, but lowering any geometry or codec allowance conservatively refuses before POST. Actual probe and full codec validation remain mandatory for every image; the 256 MiB codec setting reserves pixels and possible orientation copies, not all codec workspace.

The synchronous request allowance is 300 seconds, providing local service/body-transfer headroom for expensive work without automatically replaying it. This is not a measured service SLA, the engine's total RunLimits deadline, or proof that timeout/cancellation avoids billing. Catalog latency remains an approximate 120-second hint. The fixed request encoder uses a 4,096-byte stack buffer. The [tracked phase worksheet](decisions/2026-10-18-gemini-images.md#resource-ownership) charges known JPEG input/metadata copies and keeps every phase below the existing 628 MiB logical envelope; it does not bound allocator capacities, codec-private workspace, transport internals, concurrent host owners or RSS.

Original synthetic fixtures run offline. The ignored native smoke requires NODECRAFT_LIVE_GEMINI_IMAGES=1 plus NODECRAFT_LIVE_GEMINI_IMAGES_ACKNOWLEDGE_COST=1 and explicit spending authorization; optional second paid edit additionally requires NODECRAFT_LIVE_GEMINI_IMAGES_EDIT=1. No live test is selected in CI. Current source has not been validated against a paid service call.

Contract checked 2026-10-18: [REST](https://ai.google.dev/api/interactions-api), [image guide](https://ai.google.dev/gemini-api/docs/image-generation), [Lite](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite-image), [Nano](https://ai.google.dev/gemini-api/docs/models/gemini-nano-banana-2.1), [Pro](https://ai.google.dev/gemini-api/docs/models/gemini-3-pro-image).
## fal.ai queue models

The optional `fal` feature supports five explicit live aliases, checked against official HTTP
schemas on 2026-10-18: `fal-ai/flux/schnell`, `fal-ai/wan/v2.2-a14b/text-to-video`,
`fal-ai/kokoro/american-english`, `fal-ai/trellis-2`, and `fal-ai/esrgan`. These names are not
immutable weight versions; upstream schema/model availability can change. Hosts inject a Secret
and HTTP transport. The adapter never reads credentials, project keys or live-test variables.
The existing host provider factory reads the `fal` keychain account and constructs the adapter
off the UI thread, following the same readiness/routing path as other catalog providers.
It does not supply a desktop paid-run service or production canvas media-input resolver.

FLUX requests 512–1024 dimensions in multiples of 64, one to four PNG outputs, one to four steps
and optional u32 seed. Wan selects 480p/16fps, 17–81 frames and three aspect ratios, explicitly
turning interpolation off. Kokoro selects af_heart/am_adam and speed 0.5–2.0, returning WAV.
Trellis selects resolution 512 and one inline image, returning GLB2. ESRGAN selects scale2,
RealESRGAN_x4plus, face=false and PNG. Advanced/unknown parameters and extra roles refuse
before HTTP. Image-to-3D/upscale input contracts are direct-adapter capabilities; their engine
fixtures use an explicitly test-only immutable asset resolver. No shipping resolver is implied.

The catalog uses ByUsage USD for FLUX/Wan/Kokoro/ESRGAN: unknown is not zero/free. Every finite
budget cap refuses an unknown paid quote before invocation; an uncapped host still needs actual
run.quote/run.start partial-price consent. Trellis512 has a dated 0.25 USD per-call estimate,
verified from the [official endpoint](https://fal.ai/models/fal-ai/trellis-2/api?platform=http).
Absent monetary usage never becomes a provider-observed invoice: existing runner estimate
fallback is separate. Unknown-price completions may have no numeric money row, not free work.

Billable units (#437): fal may send `X-Fal-Billable-Units`, documented only as the "billing units
charged for this request" ([response headers](https://fal.ai/docs/documentation/model-apis/common-parameters#response-headers),
checked 2026-10-21); what a unit costs depends on the model's pricing. NodeCraft reads it from the
authenticated result fetch on the queue origin only, never from receipts, status polls or CDN
downloads. It must appear once and be a plain decimal of at most a billion with at most six
decimal places (surrounding spaces and tabs allowed). Absent, repeated, oversized or malformed,
it is left out and the result is kept. Accepted, it is recorded as `billable_units` on the job's
settings in the result's history entry (`nodecraft_types::BillableUnits`, a decimal string),
qualified by that entry's provider and model. It is observed units, never observed money: it
does not replace the unknown-price consent count, settle a charge or fill the usage log. In the
browser the header is only visible if fal exposes it to scripts (`Access-Control-Expose-Headers`);
when it does not, it reads as absent.

Accepted receipts publish a versioned handle before polling, even for already-completed work.
Persist it to resume after restart without another create. Model-specific operation roots are
explicitly allowlisted: FLUX uses fal-ai/flux, Wan fal-ai/wan, Kokoro fal-ai/kokoro, Trellis and
ESRGAN their complete model names. Request IDs are one ASCII alphanumeric/hyphen/underscore
segment, up to 128 bytes; percent escapes, slashes, dots and URL normalization aliases refuse.
Only FLUX additionally accepts the full-root status/result(/response)/cancel family printed
in the [generic queue contract](https://fal.ai/docs/documentation/model-apis/inference/queue).
Families cannot be mixed. Normal handles use admitted returned routes. Separately, a complete
bounded receipt with a safe request ID permits exactly one cleanup PUT on the canonical local
model-root `/cancel` URL if later route or metadata admission refuses. Rejected returned URLs
never gain authority. The original failure reports the safe ID and actual cleanup disposition;
malformed, duplicate, secret-bearing or missing identities permit no cleanup. These route
families compose official model HTTP roots and the generic contract, not observed paid receipts.

No ambiguous create is automatically replayed. X-Fal-No-Retry and disabled fallback are sent,
but these headers and local idempotency keys do not establish remote billing deduplication.
NodeCraft treats only the matching HTTP/body pairs 202/CANCELLATION_REQUESTED,
400/ALREADY_COMPLETED and 404/NOT_FOUND as successful best-effort cancel dispositions. Malformed,
contradictory, authentication and server replies remain errors. This is an explicit NodeCraft
policy: fal's SDK documents raising on 400/404. No disposition establishes nonbilling proof. Local poll/result/download/validation failures retain an accepted handle and unfinished
run ownership; only validated remote terminal failure ends that generation. A retained handle
cannot guarantee remote CDN retention: expired outputs can remain lost without paid replay.
No partial Completed is published after a later output fails validation.

API and CDN requests use the explicit no-redirect transport operation. Queue origin alone gets
auth; output requests carry none. CDN URLs must be HTTPS fal.media or DNS-label subdomains,
without credentials/explicit ports/fragments; bounded signed queries stay opaque and out of
errors. Other storage hosts refuse safely. Builtin browser transport refuses no-redirect
before fetch; successful wasm compilation does not imply browser fal networking support.

Operation limits are lower-only: one PNG/JPEG/WebP input at most 1 MiB / 1024²; aggregate text and
parameter strings 16 KiB; serialized request 2 MiB; API body 2 MiB; metadata 256 KiB; JSON depth 32 and
4096 items. Four output files maximum, 16 MiB each / 48 MiB total; image axes 4096 and aggregate
16,777,216 pixels. Every descriptor is admitted before downloads; actual HTTP body caps shrink
with remaining aggregate capacity. Full image codecs and actual geometry are checked; WAV,
MP4 and GLB2 receive bounded container-envelope validation, not complete audio/video/mesh codec
or playback validation. Media names/MIME alone never validate bytes. All returned files survive
in order on success. Asset/graph conversion quotas can still refuse with original paid
CompletionDelivery retained by the host; adapter errors before Completed retain only the
accepted remote handle, not a hidden partial-file cache.

Known logical-payload worksheet (MiB), with phase overlap rather than a sum across all phases:

| Phase | Known payload owners | MiB |
|---|---|---:|
| Input inspection | Inspection 2 + combined decoded/orientation rasters 128 + scratch 1 | 131 |
| Request/receipt | URI, request, native request clone, API reply, metadata and scratch | 12 |
| Result metadata | Bounded descriptors and parsing payload | 6 |
| Download validation | Validation 2 + metadata 1 + API reply 2 + outputs 48 + Vec-to-Arc live-byte overlap 16 + combined rasters 128 + scratch 1 | 198 |

A separate container/admin payload allowance of 4 gives a 202 MiB logical-payload worksheet,
not an allocation or RSS ceiling. HTTP body **length** is bounded; transport `Vec` capacity may
exceed that length through allocator growth and is additional, including its overlap with Arc
conversion. The adapter must not reject already-paid valid bytes merely because capacity grew.
Pixel/orientation objects drop before the next codec/encoder phase; 16-bit PNG and orientation
are included in known raster owners. Existing media preflight uses decoder.total_bytes plus an
orientation copy against 128 MiB; most codecs ignore their private allocation limit.
Codec-private workspace, spare transport capacity, allocator overhead, transport/TLS/kernel
buffering, caller graph/history and concurrent jobs are outside this worksheet.

HTTP errors preserve optional integer Retry-After seconds only when checked conversion fits
within one hour. Optional `X-Fal-Billable-Units` evidence is tracked in #437; it is not tokens or
money and is not invented as Usage cost.

Unit/engine/native tests use synthetic official-shape JSON and originally authored small media,
not authenticated receipts or paid output. The ignored live test requires fresh human spending
authorization, explicit --ignored, NODECRAFT_LIVE_FAL=1 and
NODECRAFT_LIVE_FAL_ACKNOWLEDGE_COST=1 BEFORE reading test-only FAL_KEY. It requests one small
FLUX image; normal CI never runs it. No paid call was used for adapter validation.
## Seedream bounds and billing

The optional `seedream` adapter uses the fixed BytePlus ModelArk image generation endpoint.
The catalog quotes **0.04 USD for one requested output** (estimate, not an actual billing receipt),
verified 2026-10-18 against [current official pricing](https://docs.byteplus.com/en/docs/modelark/model-pricing?redirect=1).
The [official image API](https://docs.byteplus.com/en/docs/modelark/image-generation-api?redirect=1)
supports generation and ordered reference editing. This adapter requests one nonstreaming base64
JPEG, with watermark **always enabled**; there is no option to disable it. Sequential batches
and newer 5.0 controls are unsupported. Keys belong in the host keychain, never project or error
text. Constructing an adapter does not read credentials.

Up to **14 ordered reference occurrences**, including duplicates, are supported subject to all
input limits: PNG/JPEG/WebP, 8 MiB each, 48 MiB combined, 16,777,216 pixels per image and
**32,000,000 pixels combined**. Fourteen 2048×2048 references exceed the combined pixel limit and
are refused locally. Both input dimensions must exceed 14 and aspect ratio must not exceed 16.
The prompt limit is 128,000 bytes and 32,000 characters. Output is one JPEG up to 8 MiB,
16,777,216 pixels and ratio 16; the derived 16,384-axis limit admits wide 4K shapes. Request,
response and metadata limits are 64 MiB, 13 MiB and 256 KiB. Limits can only be lowered; lowered
output geometry that cannot admit server-selected shapes refuses before sending.

All references are probed before sequential full decoding, and decoded pixels are dropped before
encoding. Local validation/encoding refusals carry affirmative nonacceptance and release budget
reservations. Immediately before invoking transport, delivery becomes uncertain: lost replies,
HTTP errors, cancellation/deadlines and malformed paid responses can still incur cost. There is
no documented submit deduplication or resumable handle, so automatic paid replay is disabled.

Response admission deliberately requires the exact dated model ID, unique required size metadata
matching decoded dimensions, and exactly one complete JPEG with no bytes after EOI. Model aliases,
missing size or trailing data are refused rather than guessed or silently truncated; such refusals
after sending may still be billed. Malformed optional usage preserves an otherwise valid image
with unknown usage; token counts and catalog estimates never become fabricated actual charges.
The [logical memory worksheet](decisions/2026-10-18-seedream-local-delivery-and-resource-limits.md)
records phase owners and exclusions; codec workspace and process RSS are not universally bounded.

All HTTP uses the explicit no-redirect contract, with no fallback. Browser Reqwest refuses it, so
wasm needs an audited host transport. The adapter does not follow/download result URLs. Direct
adapter multi-reference edits are supported; existing graph Media→ProviderJob conversion still
needs an asset resolver, so end-to-end canvas editing is not claimed. Hosts retain Completed if
output routing/conversion refuses it.

The ignored live test requires NODECRAFT_LIVE_SEEDREAM=1,
NODECRAFT_LIVE_SEEDREAM_ACKNOWLEDGE_COST=1 and ARK_API_KEY, plus separate explicit human spending
authorization. It never runs in CI; ordinary contracts use authored fixtures and local HTTP.

## Tripo 3D tasks

The optional `tripo` adapter drives the [Tripo v2 API](https://docs.tripo3d.ai) at the fixed base
`https://api.tripo3d.ai/v2/openapi`. Three catalog models share it, one per task type, and appear on
the **Generate 3D** card (`generation.mesh`, which also offers fal's Trellis):

| Catalog model | Task | Inputs | Estimate |
|---|---|---|---|
| `tripo.text-to-3d` | `text_to_model` | a prompt (1–1,024 characters), optional negative prompt (255) | 0.20 USD |
| `tripo.image-to-3d` | `image_to_model` | one image | 0.30 USD |
| `tripo.multiview-to-3d` | `multiview_to_model` | a front view plus one to three of left, back, right | 0.30 USD |

Estimates were verified 2026-10-21 (1 credit = 0.01 USD) and are per task, not actual charges.
They cover a textured PBR model at standard quality, so the adapter always sends `texture` and
`pbr` as true and exposes no setting that would change the price (detailed textures, quad meshes,
smart low-poly). The settings it does expose are `model_version` (v2.5, v3.0 or v3.1, dated),
shape and texture seeds and a face limit (1,000–1,500,000).

Images (PNG, JPEG or WebP whose bytes match their type, at most 20 MB each) are uploaded first;
each upload returns a token and creates no task, so a failed upload is a proved non-acceptance.
The task request comes next and is the paid step: from then on a lost reply may have billed, and
there is no idempotency key, so a submit is never retried automatically. Polling is every three
seconds and reports Tripo's queue position and percentage. Tripo has no cancel endpoint, so
cancelling a run stops waiting but not the task.

A finished task names short-lived (about five minutes) result links. The adapter takes the PBR
model, then the plain or base model, and downloads it **without the key**, from public `https`
hosts only (no IP literals, single-label or internal names such as `*.internal` or `*.local`,
ports or credentials in the link). A failed download is retryable: the next poll hands out a
fresh link, so an expired one never loses a billed model. The download must be a whole glTF 2.0
binary (`glTF` magic, version 2, length matching) of at most 256 MiB; anything else is refused as
malformed, and since the task already ran it may still have been billed. Error replies map
Tripo's codes onto the shared kinds (auth, rate limit and quota with `Retry-After`, content
policy, credit, input errors, server); their messages are cut short and withheld if they echo
the key. Limits can only be lowered.

The multiview task needs the front view and at least one other; the card accepts a front view
alone and the adapter refuses it before anything is uploaded. Tripo is native only: the adapter
fetches results with no-redirect requests, which the builtin browser transport refuses.

**Not verified live yet:** the model files Tripo returns are expected to be plain GLBs. A GLB that
needs an extension the mesh decoder doesn't read (meshopt or Draco compression, say) still
lands in the card's history, but its viewer and the 3D captures report it can't be read.

The ignored live test requires `NODECRAFT_LIVE_TRIPO=1`, `NODECRAFT_LIVE_TRIPO_ACKNOWLEDGE_COST=1`
and `TRIPO_API_KEY`, plus separate explicit human spending authorization (about 0.20 USD). It
never runs in CI:

```sh
NODECRAFT_LIVE_TRIPO=1 NODECRAFT_LIVE_TRIPO_ACKNOWLEDGE_COST=1 TRIPO_API_KEY=… \
  cargo test -p nodecraft-providers --features http,tripo --test live_tripo -- --ignored
```

## Meshy 3D tasks

The optional `meshy` adapter drives the [Meshy API](https://docs.meshy.ai/en/api) at the fixed base
`https://api.meshy.ai/openapi` (Text to 3D on `v2`, the other tasks on `v1`). Six catalog models
share it and appear on the **Generate 3D** card:

| Catalog model | Task | Inputs | Estimate |
|---|---|---|---|
| `meshy.text-to-3d` | `text-to-3d` (preview) | a prompt (1–800 characters) | 0.40 USD |
| `meshy.image-to-3d` | `image-to-3d` | one image | 0.60 USD |
| `meshy.multi-image-to-3d` | `multi-image-to-3d` | a front view plus up to three of left, back, right | 0.60 USD |
| `meshy.retexture` | `retexture` | a wired GLB model and a prompt | 0.20 USD |
| `meshy.retexture-image` | `retexture` | a wired GLB model and a style image | 0.20 USD |
| `meshy.remesh` | `remesh` | a wired GLB model | 0.10 USD |

Estimates were verified 2026-10-21 at 0.02 USD a credit (the Pro plan's rate; other plans pay a
little more or less per credit) and are per task, not actual charges. The adapter fixes
everything that changes the price: Text to 3D asks for the untextured preview stage (20 credits);
image and multi-image ask for textured PBR models with 2K textures (30 credits); retexture paints
2K PBR textures (10 credits); remesh is 5. The settings it exposes are the model (Meshy 6 or 7.1,
the same price; `latest` or Meshy 6 for retexture), a pose (A or T) for generation, keeping the
model's UVs when retexturing, and remesh topology (triangles or quads) and target face count
(100–300,000).

Every input travels inline in the one task request as a data URI: images (PNG, JPEG or WebP whose
bytes match their type, at most 15 MiB each before encoding, inside Meshy's 20 MB cap) and models
(a whole glTF 2.0 binary of at most 64 MiB; convert other 3D formats to GLB first). The task
request is the paid step: a lost reply may have billed and Meshy takes no idempotency key, so a
submit is never retried automatically. Polling is every three seconds and reports Meshy's queue
position and percentage. A task can't be cancelled: Meshy's delete erases the task and its
models without a documented refund, so cancelling says the task runs to the end and is billed.

A finished task's GLB link is downloaded **without the key**, from public `https` hosts only,
with the same checks as Tripo's (no IP literals, internal names, ports or credentials; a whole
glTF 2.0 binary of at most 256 MiB; a failed download retried on the next poll's fresh link).
Error replies map HTTP statuses onto the shared kinds (400/422 input, 401/403 auth, 402 credit,
404, 429 with `Retry-After`, 5xx server or busy) with Meshy's message cut short, and withheld if
it echoes the key. A failed task reports Meshy's reason. Limits can only be lowered. Meshy is
native only for the same reason as Tripo.

**Not verified live yet:** Meshy's data URI support for models is documented but untested here,
and as with Tripo a returned GLB that needs a mesh decoder extension lands in history but can't
be viewed.

The ignored live test requires `NODECRAFT_LIVE_MESHY=1`, `NODECRAFT_LIVE_MESHY_ACKNOWLEDGE_COST=1`
and `MESHY_API_KEY`, plus separate explicit human spending authorization (about 0.40 USD). It
never runs in CI:

```sh
NODECRAFT_LIVE_MESHY=1 NODECRAFT_LIVE_MESHY_ACKNOWLEDGE_COST=1 MESHY_API_KEY=… \
  cargo test -p nodecraft-providers --features http,meshy --test live_meshy -- --ignored
```
