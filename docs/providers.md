# Providers

Model services NodeCraft talks to, how to set them up, and what they can do. Each provider is a
module in `crates/providers` behind a Cargo feature (CONTRIBUTING.md, "Adding a model or
provider"). API keys live in the OS keychain (#80), never in project files or logs.

| Provider | Feature | Auth | Modes | Notes and limits |
|---|---|---|---|---|
| **OpenAI** (Chat Completions) | `llm` | API key (`Authorization: Bearer`) | text → text, image + text → text, streaming | No idempotency keys on chat: a retried prompt may be billed again. |
| **OpenAI Images** | `openai-images` | supplied OpenAI API key (`Authorization: Bearer`) | generate, ordered references/edit, first-image alpha-mask inpaint, sizes/quality/transparency/N | Two dated GPT Image 2.5 models; usage price unknown; no ambiguous automatic submit replay, remote cancellation or builtin browser transport. See below. |
| **Replicate** | `replicate` | supplied bearer token | pinned SDXL generation, img2img, mask inpaint; pinned scalar text | Versioned resumable predictions, bounded credential-free delivery downloads; unknown usage price. See below. |
| **Ollama** | `llm` | none (local, `http://127.0.0.1:11434`) | text, vision models, streaming | OpenAI-compatible `/v1`; models listed from `/v1/models`. Pull a model first. |
| **LM Studio** | `llm` | none (local, `http://127.0.0.1:1234`) | text, vision models, streaming | OpenAI-compatible server. |
| **vLLM / other gateways** | `llm` | optional bearer key | text, streaming | Any OpenAI-compatible base URL (`Endpoint::openai_compatible`). |
| **OpenRouter** | `llm` | API key | text, vision, streaming | Model ids like `anthropic/claude-opus-5-5`. |
| **LiteLLM** | `llm` | virtual key (bearer) | as configured on the proxy | Spend per request from `x-litellm-response-cost` (reaches budgets as the job's `Usage::cost`); a key's total spend and budget from `/key/info` (`LlmClient::litellm_key_info`); models from `/v1/models`. |
| **Anthropic** (Messages) | `llm` | API key (`x-api-key`, `anthropic-version: 2023-06-01`) | text, image + text → text, streaming | Default model `claude-opus-5-5`. Current models reject sampling parameters, so `temperature` is only sent when set; thinking is adaptive and not configured. A `refusal` stop is reported as blocked content. |
| **ComfyUI** | `comfyui` | none locally; optional auth header for servers behind a proxy | whatever the workflow does (image, video, audio, 3D) | Jobs carry an API workflow, `<node>.<input>` value overrides and media inputs (uploaded under content-hash names). Missing custom nodes and models are reported before queueing. Prompt ids are salted per install; retries never queue twice. Only `output` files are results (previews aren't kept). Progress is queue position and completion until the WebSocket lands. Servers too old for prompt-scoped `/interrupt` stop whatever runs when you cancel. |
| **Seedream 4.5 (BytePlus ModelArk)** | `seedream` | API key (`Authorization: Bearer`) | text → one image; direct-adapter ordered multi-reference editing | Dated `seedream-4-5-251128`; one JPEG, 2K/4K, up to 14 references. No automatic paid replay or remote cancellation; no builtin browser no-redirect transport. |

## Setting up catalog providers

The engine builds an adapter for every provider the model catalog names
(`engine::provider_setup`, #409). Each paid adapter reads its key from the keychain account below;
ComfyUI reads its server from `config.toml`. An adapter that can't be built is reported with what
to do, and a node routed to it is refused before planning, so no request is sent.

| Catalog provider | Keychain account | Also needs |
|---|---|---|
| `openai-images` | `openai` | — |
| `replicate` | `replicate` | — |
| `seedream` | `seedream` | — |
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
2 MiB, 64 non-input object fields plus 32 fields in the ignored root input subtree, 256 aggregate
values and depth 16. A borrowed lexical pass separately budgets the
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
