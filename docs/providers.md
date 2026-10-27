# Providers

Model services NodeCraft talks to, how to set them up, and what they can do. Each provider is a
module in `crates/providers` behind a Cargo feature (CONTRIBUTING.md, "Adding a model or
provider"). API keys live in the OS keychain (#80), never in project files or logs.

| Provider | Feature | Auth | Modes | Notes and limits |
|---|---|---|---|---|
| **Deepgram** | `deepgram` | API key (`Authorization: Token`), keychain account `deepgram` | English audio file → text | Nova-3, smart formatting, unknown usage cost, no uncertain replay. See below. |
| **Black Forest Labs** | `bfl` | BFL API key in account `bfl` (`x-key`) | FLUX.2 klein 4B text → PNG | Direct queued generation with resumable cluster polling, bounded downloads and unknown usage price; no automatic ambiguous submit replay or remote cancellation. See below. |
| **OpenAI** (Chat Completions) | `llm` | API key (`Authorization: Bearer`) | text → text, image + text → text, streaming | No idempotency keys on chat: a retried prompt may be billed again. Builtin catalog (`assets/catalog/openai.toml`, #503): GPT-6.1 Sol, GPT-6 Luna, GPT-4.1 mini, text → text and up to 16 images + text → text (#592), quoted at the output price on `max_tokens` (input billed on top; GPT-6.1 Sol and GPT-6 Luna at their 1.5x rate for prompts over 272K tokens, so the quote stays an upper bound); only GPT-4.1 mini offers `temperature`. |
| **OpenAI Audio** | `openai-audio` | existing `openai` API key (`Authorization: Bearer`) | text → MP3, audio file → text | Unknown upfront usage price; no automatic paid replay. Speech model shuts down January 6, 2027; Realtime is not supported. See below. |
| **Hugging Face Inference Providers** | `huggingface` | HF token with Inference Providers permission, keychain account `huggingface` | text → text, streaming | GPT OSS 120B through the explicit Groq route; direct router with the existing chat client, unknown usage price, no ambiguous replay. See below. |
| **OpenAI Images** | `openai-images` | supplied OpenAI API key (`Authorization: Bearer`) | generate, ordered references/edit, first-image alpha-mask inpaint, sizes/quality/transparency/N | Two dated GPT Image 2.5 models; usage price unknown; no ambiguous automatic submit replay, remote cancellation or builtin browser transport. See below. |
| **Gemini Images** | `gemini-images` | supplied Gemini API key (`x-goog-api-key`) | generation and ordered reference editing | Three stable Gemini models, unknown usage price; bounded inline JPEG outputs. |
| **Gemini Video** | `gemini-video` | supplied Gemini API key (`x-goog-api-key`), same `gemini` account | Omni Flash text, first/last frame and reference images → video with sound | Veo on the Gemini API is shut down; Veo now runs on Google Cloud (`google-veo`). Paid tier only; charged by measured tokens, no price before the run. See below. |
| **Veo on Google Cloud** | `google-veo` | a service-account key file, kept compact in the `google-cloud` account; a signed JWT is exchanged for an hour's OAuth token (`Authorization: Bearer`) | Veo 3.1, Fast and Lite: text → video with sound. Veo 3.1 and Fast also: first/last frame, and up to three subject references | Runs in the account's own project in `us-central1`. Quoted per second by resolution before the run. Never replayed after an ambiguous submit; no remote cancellation. See below. |
| **Google TTS** | `google-tts` | the same `google-cloud` service-account key as Veo (`Authorization: Bearer`, billed to the account's project) | text → speech (MP3): Chirp 3 HD, Neural2, WaveNet, Standard and Studio voices with speaking rate, pitch and SSML; Gemini 2.5 Pro, Flash, Flash-Lite and 3.1 Flash voices with a style prompt and two-speaker dialogue | One synchronous request per line. Classic voices are priced per character, and the cost is counted from the characters sent once the run is done; Gemini voices are billed by tokens Google doesn't report, so no price before the run. Never replayed; nothing to cancel. See below. |
| **Gemini images on Google Cloud** | `google-gemini-images` | the same `google-cloud` service-account key as Veo (`Authorization: Bearer`, billed to the account's project) | Gemini 3.1 Flash Image, 3 Pro Image, Nano Banana 2.1, 3.1 Flash-Lite Image and 2.5 Flash Image: text → image and up to 14 reference images (3 on 2.5) → image, with aspect ratio and 1K/2K/4K size | One synchronous `generateContent` request on the global endpoint. Quoted per image by size before the run; charged by the tokens the reply measured. Never replayed; nothing to cancel. See below. |
| **Replicate** | `replicate` | supplied bearer token | pinned SDXL generation, img2img, mask inpaint; pinned scalar text | Versioned resumable predictions, bounded credential-free delivery downloads; unknown usage price. See below. |
| **Ollama** | `llm` | none (local, `http://127.0.0.1:11434`) | text, vision models, streaming | OpenAI-compatible `/v1`; models listed from `/v1/models`. Pull a model first. |
| **LM Studio** | `llm` | none (local, `http://127.0.0.1:1234`) | text, vision models, streaming | OpenAI-compatible server. |
| **vLLM / other gateways** | `llm` | optional bearer key | text, streaming | Any OpenAI-compatible base URL (`Endpoint::openai_compatible`). |
| **OpenRouter** | `llm` | API key | text, vision, streaming | Model ids like `anthropic/claude-opus-5-5`. |
| **LiteLLM** | `llm` | virtual key (bearer) | as configured on the proxy | Spend per request from `x-litellm-response-cost` (reaches budgets as the job's `Usage::cost`); a key's total spend and budget from `/key/info` (`LlmClient::litellm_key_info`); models from `/v1/models`. |
| **Azure OpenAI** (v1 Chat Completions) | `llm` | resource API key (`Authorization: Bearer`) | text chat and streaming | Reuses `LlmClient` through `Endpoint::azure_openai`; the **Azure OpenAI deployment** card sends the configured deployment name as `model`. Unknown usage price requires explicit acceptance. No dated `api-version` or Azure SDK. |
| **Anthropic** (Messages) | `llm` | API key (`x-api-key`, `anthropic-version: 2023-06-01`) | text, image + text → text, streaming | Default model `claude-opus-5-5`. Builtin catalog (`assets/catalog/anthropic.toml`, #503): Opus, Sonnet and Haiku 5.5, text → text and up to 16 images + text → text (#592), quoted at the output price on `max_tokens` (input, images included, billed on top). Each image may be at most 7.5 MB (10 MB once base64-encoded); a larger one is refused before sending. Current models reject sampling parameters, so `temperature` is only sent when set; thinking is adaptive and not configured. A `refusal` stop is reported as blocked content. |
| **ComfyUI** | `comfyui` | none locally; optional auth header for servers behind a proxy | whatever the workflow does (image, video, audio, 3D) | The catalog's FLUX.1 [dev] and SDXL cards run a built-in workflow of core nodes (text-to-image at the card's aspect ratio, or image-to-image from an image scaled to about one megapixel, keeping its shape) with the checkpoint chosen on the card; a checkpoint the server doesn't list is named before anything is uploaded. A ComfyUI workflow card (#9, model `comfyui.workflow`, free) carries the workflow imported onto it as a `comfy.workflow` text input and its parameter values as a `comfy.values` text input (a JSON object of `<node>.<input>` to a string, number or boolean); a `comfy.outputs` text input names the output nodes it keeps, and the workflow's other output nodes are dropped before queueing; all three are bounded at 1 MiB and checked, with the workflow, before anything is uploaded. `comfy.workflow.import` places a workflow file (API or UI format) as such a card, read against the server's node list that `comfy.nodes.refresh` (palette: Read ComfyUI server nodes) fetches; dropping the file or pasting its JSON on the canvas does the same. Pasted workflow text is titled Pasted workflow because the text clipboard supplies no filename; a file drop keeps its filename. Several servers can be set up at once (#864): each has a name, an address and an optional cap on how many of a run's jobs it takes at once, and one is the default. A ComfyUI card runs on the default unless `comfy.server.use` (the card menu's **ComfyUI server**) names another; a card naming a removed server follows the default. Each server is its own provider instance with its own scheduler lane, so two cards on one canvas run on two machines, and a job restored after a restart goes back to the server it was sent to. In the desktop app the toolbar's **ComfyUI** panel adds, edits, removes and checks servers and picks the default: a check (`comfy.nodes.refresh {server}`) reads a server's nodes and reports its ComfyUI version, first device with free VRAM, and queue (`/system_stats`, `/queue`). Only the default's node list is kept for importing workflows. Changing or removing a server drops only that server's adapter and what was read from it. Other jobs carry an API workflow, `<node>.<input>` value overrides and media inputs (uploaded under content-hash names with the extension the loader lists, e.g. `.mov`, `.mkv`, `.mp3`; a file a loader can't read, such as a sound for `LoadImage`, is refused before anything is uploaded). Video and audio loaders and savers map to Video and Audio, including the video helper suite's `VHS_LoadVideo`, whose upload `/object_info` doesn't flag (#110). Missing custom nodes and models are reported before queueing. Prompt ids are salted per install; retries never queue twice. Only `output` files are results (previews aren't kept), up to 256 MiB per run, the most a run keeps. The returned bytes are sniffed, so an animated WebP lands on an image port. Polling reports queue position and completion; on the desktop and CLI the server's WebSocket (plain `ws://`, opened before each prompt is queued and reopened if lost) adds how far a running prompt is and which node runs. An `https://` server, the browser, or a proxy that refuses upgrades falls back to polling, and a run resumed after a restart shows no fraction until it ends. Servers too old for prompt-scoped `/interrupt` stop whatever runs when you cancel. |
| **fal.ai** | `fal` | host-supplied key (`Authorization: Key`) | FLUX image/styles, Ideogram image, Wan, Luma Ray and LTX video, Luma Photon image, Kokoro audio, Trellis 3D, ESRGAN upscale, Bria background removal | Explicit live aliases; queue polling/resume, bounded whole-output downloads and best-effort cancellation. Dated estimates for Ideogram, Trellis, style training, image-only Bria and Ray; other quotes are by usage. |
| **Seedream 4.5 (BytePlus ModelArk)** | `seedream` | API key (`Authorization: Bearer`) | text → one image; ordered multi-reference editing | Dated `seedream-4-5-251128`; one JPEG, 2K/4K, up to 14 references. No automatic paid replay or remote cancellation; no builtin browser no-redirect transport. |
| **Seedance 2.5 and 2.0 (BytePlus ModelArk)** | `seedance` | the Seedream key (`Authorization: Bearer`) | text, a first (and last) frame or reference images → video with sound | 2.5, 2.0, 2.0 Fast and 2.0 Mini, dated; 4–30 s (2.5) or 4–15 s, 480p to 1080p (4K on 2.0). Quoted per second at an upper bound; billed only when a video succeeds. Cancel while queued; no automatic paid replay. See below. |
| **Tripo** | `tripo` | API key (`Authorization: Bearer`) | text → 3D, image → 3D, multiview (front/left/back/right) → 3D | One textured PBR GLB per task on a Generate 3D card; dated per-task estimates. No remote cancellation and no automatic paid replay. See below. |
| **Meshy** | `meshy` | API key (`Authorization: Bearer`) | text → 3D, image → 3D, multi-image → 3D, retexture and remesh a wired GLB | One GLB per task on a Generate 3D card; dated per-task estimates. Tasks can't be cancelled. No automatic paid replay. See below. |
| **Higgsfield** | `higgsfield` | key id and secret, stored as `id:secret` (`Authorization: Key`) | SOUL 2 text → 1 or 4 images, optionally with an account Soul ID; Kling 2.5 Turbo Pro image → video; Genjutsu motion transfer and object swap (video + images → video); Soul ID training (images → identity) | Credits priced by plan, so SOUL and Kling prices are unknown; the adapter can ask Higgsfield for an estimate. Genjutsu is quoted per second of source at list price, an upper bound. Idempotent submits, replayed after a lost reply; cancel while queued. See below. |
| **Runway** | `runway` (feature `runway`) | API key (`Authorization: Bearer`, keychain account `runway`) | Gen-4.5 text or first frame → video; Gen-4 Turbo first frame → video | Standard MP4, 2–10 seconds. 0.12/0.05 USD per second before tax. No submission deduplication; uncertain creates are never replayed. See below. |
| **Kling** | `kling` (feature `kling`) | API key (`Authorization: Bearer`), or access key and secret key stored as `ak:sk`, which sign a 30-minute HS256 token per request | Kling 2.5 Turbo, 2.1 Master and 1.6 text → video and start frame (+ end frame) → video; Kling 1.6 up to four references → video; Kling Image 2.1 text → image | Billed in prepaid Kling units, so no quote before the run. One create per job carrying an `external_task_id` from the idempotency key; never replayed automatically; no remote cancellation. See below. |
| **Comfy Cloud** | `comfy-cloud` (feature `comfy-cloud`) | Comfy API key (`Authorization: Bearer` on v2, `X-API-Key` on `/api/object_info`) | whatever the workflow does, on Comfy-hosted GPUs | The same job shape as ComfyUI. Inputs upload as assets; one idempotency key per run, never charged twice; no automatic paid replay. Partner (API) nodes are not supported. See below. |
| **Stability AI** | `stability` (feature `stability`) | Bearer API key, keychain account `stability` | Core text → image, Audio 3 Large text → audio, Fast 3D image → GLB | Direct first-party APIs; dated fixed-call estimates. Audio polls/resumes one accepted id; no submission deduplication or confirmed cancellation. Desktop/CLI only. See [details](#stability-ai-direct). |

## Retired APIs

**OpenAI Sora (#28):** as verified on October 24, 2026, OpenAI's Videos API and Sora 2 models
were shut down on September 24, 2026. The official
[deprecation notice](https://developers.openai.com/api/docs/deprecations#2026-03-24-sora-2-video-generation-models-and-videos-api)
lists no replacement. NodeCraft has no Sora adapter or catalog entry; this retired API is not
an available generation option. Other video adapters retain their own documented models,
credentials and pricing. Any future Sora integration needs a newly verified supported API.

## Image facts on inputs and outputs

An imported image's reference says whether it has an alpha channel, its stored bits per channel
and its colour-space tag, all verified against its bytes when the project is saved or opened
([decision](decisions/2026-10-21-image-header-metadata.md)). A field can be absent, which means
unknown, and outputs written by provider adapters record only their size for now. So an adapter
deciding an alpha or format policy probes the bytes it sends, as every adapter below already
does, and never treats an absent field as "opaque 8-bit sRGB".

## Setting up catalog providers

The engine builds an adapter for every provider the model catalog names
(`engine::provider_setup`, #409). Each paid adapter reads its key from the keychain account below;
ComfyUI and LiteLLM read their servers from `config.toml`, or the browser's per-site Providers
panel (#721). An adapter that can't be built is reported with what
to do, and a node routed to it is refused before planning, so no request is sent.

| Catalog provider | Keychain account | Also needs |
|---|---|---|
| `openai` (chat) | `openai` | — |
| `anthropic` (chat) | `anthropic` | — |
| `azure-openai` (chat) | `azure-openai` | `[providers] azure_openai_url` with an HTTPS Azure resource root or its `/openai/v1` base |
| `openai-images` | `openai` | — |
| `replicate` | `replicate` | — |
| `seedream` | `seedream` | — |
| `seedance` | `seedream` (the same ModelArk key) | each Seedance model activated in the ModelArk console |
| `tripo` | `tripo` | — |
| `meshy` | `meshy` | — |
| `higgsfield` | `higgsfield` (the key id and secret joined as `id:secret`) | — |
| `kling` | `kling` (an API key, or the access key and secret key joined as `ak:sk`) | — |
| `elevenlabs` | `elevenlabs` | — |
| `google-veo` | `google-cloud` (paste the whole service-account key file; it is stored compactly) | the Vertex AI API enabled, and the Vertex AI User role on the account's project |
| `google-tts` | `google-cloud` (the same key as `google-veo`) | the Cloud Text-to-Speech API enabled in the account's project; Gemini voices also need the Vertex AI User role |
| `google-gemini-images` | `google-cloud` (the same key as `google-veo`) | the Vertex AI API enabled, and the Vertex AI User role on the account's project |
| `comfy-cloud` | `comfy-cloud` | — |
| `comfyui` | `comfyui:<host>:<port>`, or `comfyui:<host>` when the address names no port (optional bearer token) | `[[providers.comfyui]]` entries with `name`, `url` (such as `"http://127.0.0.1:8188"`) and optional `max_jobs` (1–16; unset, 4), and `[providers] comfyui_default` naming one (or the desktop toolbar's ComfyUI panel), and the per-install `install_salt`; an older `comfyui_url` is read as one server |

LiteLLM's endpoint and optional `litellm` virtual key can be saved now, but the shipped model
catalog has no LiteLLM entries: no card can select or call the proxy yet. The adapter accepts the
proxy root (`[providers] litellm_url = "https://proxy.example"`, without `/v1`) for future catalog
integration. Saving this address alone does not add models.
The model-catalog integration decision is tracked in [#489](https://github.com/CypherNaught-0x/nodecraft/issues/489).

Select **Azure OpenAI deployment** on a text generation card and enter your exact Azure
deployment name in **Azure deployment name**. NodeCraft accepts 1–64 ASCII letters, digits,
hyphens, underscores or dots and sends that value unchanged as `model`; the name need not
match the underlying model. Blank or malformed names refuse before any request. The card
offers text input/output and an output-token cap; the selected deployment must support v1
Chat Completions. Its generic quality/latency values are discovery hints only.

No universal Azure price is assumed. The catalog uses `ByUsage`, so the picker says
**Usage based (USD)** and run confirmation requires explicit unknown-price acceptance.
Azure returns token usage, not a monetary charge: this card cannot settle an exact dollar
cost. An unknown-price acceptance is not a monetary ceiling. Custom hosts with a validated
catalog can instead use actual rates and a literal deployment name as `provider_model`.
NodeCraft does not load arbitrary catalog files at runtime. The browser has no Azure endpoint
settings panel; native desktop/CLI configuration is supported below.

For native desktop/CLI use, save the resource key through `keys.set` with account
`azure-openai` (or the API keys panel), and set the non-secret endpoint in settings:

```toml
[providers]
azure_openai_url = "https://YOUR-RESOURCE.openai.azure.com"
```

The preset also accepts `https://YOUR-RESOURCE.services.ai.azure.com/openai/v1/`. It refuses
HTTP, embedded credentials, queries, fragments and legacy `/openai/deployments/...` paths.
It sends the shared Chat Completions request to `/openai/v1/chat/completions`, with the
output cap as `max_completion_tokens` (including reasoning tokens); omit `temperature` for
deployments that reject it. API-key authentication is supported; Entra login/token refresh,
Responses API, model discovery as deployment discovery and dated preview endpoints are not.
This follows Microsoft's [v1 migration guide](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle?tabs=key)
and [Chat Completions reference](https://learn.microsoft.com/en-us/rest/api/microsoft-foundry/azureopenai/chat),
checked 2026-10-24.

Chat completes at submit; polling has no remote job and cancellation cannot undo an accepted
request. Streaming text, token usage, bounded replies and error mapping reuse the shared
adapter. No idempotency guarantee is advertised: Azure disables automatic retries of an
uncertain submission, since manually retrying can cost again. Offline contract tests cover endpoint construction, deployment names,
auth secrecy, usage, streaming, content filtering, errors and the synchronous lifecycle.
The opt-in smoke test costs tokens and was not run for this addition:

```sh
NODECRAFT_LIVE_AZURE_OPENAI=1 AZURE_OPENAI_API_KEY=… \
  AZURE_OPENAI_ENDPOINT=https://YOUR-RESOURCE.openai.azure.com \
  AZURE_OPENAI_DEPLOYMENT=YOUR-DEPLOYMENT \
  cargo test -p nodecraft-providers --test live_llm live_azure_openai
```

Keys are read when an adapter is first built, which can show a keychain prompt, so hosts build
adapters off the UI thread and only for the providers a run uses; each run gets a fresh routing
from the adapters already built. ComfyUI's token is optional: a keychain that can't be opened or
was refused means no token, not no ComfyUI. The app makes `install_salt` once and saves it at once;
with read-only settings none is made and ComfyUI stays off.

Older Workflow cards may lack stored partner-node names. In the desktop, refreshing their
selected server installs runtime evidence from `/object_info`, so quotes and budget admission
treat matching partner nodes as unknown paid work. Identical refreshes and checks of unrelated
profiles preserve an open quote. This evidence is not persisted in the project: before a refresh
in a new session, and in CLI, MCP or serve hosts that do not install the desktop refresh result,
legacy cards retain their previous pricing behavior. Reimporting with a current server list
stores partner names on the card and avoids that host/session limitation.

In the browser, the same server addresses and installation salt persist only for this origin,
separately from API keys. Open Providers to edit them; a failed save keeps the previous settings.
ComfyUI and LiteLLM must allow this page's origin in their CORS settings, and an HTTPS page may
block an HTTP endpoint. `options.refresh` runs asynchronously on the page and hands its option
lists to the next frame; changing server addresses cancels an old endpoint's pending list.

### Testing a key

`keys.test {account}` (the keys panel's **Test** button, #237) checks the stored key with one GET
that costs nothing. The key is read from the keychain only (never the environment), sent as a
secret header to the provider's own API with redirects not followed, and never logged or put in an
error; replies are never quoted. The answer comes from the HTTP status alone:

- `{result: "verified", masked}`: the provider answered a request that needs a valid key. Checked
  this way: `openai` (model list), `anthropic` (model list), `gemini` (model list), `replicate`
  (account), `elevenlabs` (voice list), `tripo` (balance), `meshy` (balance), `higgsfield` (Soul ID
  list), `openrouter` (the key's own details).
- `{result: "unverifiable", reason, masked}`: `ollama` and `lmstudio` take no key; every other
  account (`fal`, `seedream`, `kling`, `google-cloud`, `comfy-cloud`, `comfyui:*`) has no known free
  check, so nothing is sent.
- **Rejected:** a 401 (or Google's `API_KEY_INVALID`) is an `Authentication` error whose message
  starts with `rejected:`. A 403 is one too, starting `rejected or not allowed:`, since a valid key
  can be refused for its region, a disabled API or scoped permissions. Rate limiting is `Busy`;
  redirects, other statuses, a reply that isn't JSON and network failures are `Network` or
  `Timeout` errors that claim no verdict (only a 5xx suggests trying again later); a missing key is
  `NotFound`.

The check runs on the keychain thread using the runs' runtime and HTTP client, so the window never
waits. The keychain thread waits for the check, though, so while a test is out (at most 25 seconds,
for a slow or unreachable provider) other key requests, such as **Save**, **Check** or **Remove**,
wait behind it. Hosts without the run host (`nodecraft-cli`, the browser, or a desktop app whose
run host couldn't start) answer `Unsupported` at once. MCP clients can't run it (see
[MCP](mcp.md)).

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

## Provider quotes

A catalog model priced `by_usage` has no price before it runs. When its provider can say what a job
would cost, `Provider::quote` answers with no idempotency key and spends nothing; the default
answer is "unsupported". `run.estimate` (same params as `run.quote`) asks the run host to build
each by-usage job exactly as the run would send it and quote it on the same provider instance; the
answers arrive as `RunNotice::Estimated`, carrying the request id the command returned, and are
kept by the session for that provider routing for ten minutes. Asking sends the job's inputs, so an
image is uploaded to the provider before anyone accepts the run; nothing is charged.
`run.quote` and `run.start` then price a by-usage step at the dearest of its jobs' quotes, when
every one of its jobs was quoted, list it in `quoted`, and check budgets against it. A job whose
quote failed, timed out (60 s) or wasn't asked (its inputs are made earlier in the same run) keeps
the step unknown before execution. New quotes change the quote token, so a run quoted before
them must be quoted again. The desktop confirmation asks for estimates by itself when part of the
price is unknown (also when a budget refuses that unknown part), says the inputs are sent while it
asks, then re-prices. Mock fixtures cover this (`MockProvider::with_quote`); nothing
here calls a real provider in tests. Native CLI `quote`/`run --estimate` and standalone MCP also
wait for their matching terminal estimate before the next quote; see [MCP server](mcp.md#tools)
for the result fields, input-upload behavior and timeout.

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
the library feature still compiles for wasm. The shipping RunHost resolves wired image assets
for catalog editing; an offline real-adapter regression covers this path alongside Gemini and Seedream.

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

## Google TTS (`google-tts` feature)

`GoogleTts` speaks text with Cloud Text-to-Speech (#21), signed in with the same service account
as Veo (see below). Enable the *Cloud Text-to-Speech API* in the account's project; Gemini voices
also need the *Vertex AI User* role. Each line is one `POST
https://texttospeech.googleapis.com/v1/text:synthesize`, billed to the account's project
(`x-goog-user-project`), and returns one MP3 inline.

- **Classic voices**, one model per family because each family has its own price:

  | Model | Voices | Price per 1M characters | Rate | Pitch |
  |---|---|---|---|---|
  | `google-tts.chirp3-hd` | `en-US-Chirp3-HD-…` | 30 USD | 0.25–2 | — |
  | `google-tts.neural2` | `en-US-Neural2-…` | 16 USD | 0.25–4 | ±20 |
  | `google-tts.wavenet` | `en-US-Wavenet-…` | 4 USD | 0.25–4 | ±20 |
  | `google-tts.standard` | `en-US-Standard-…` | 4 USD | 0.25–4 | ±20 |
  | `google-tts.studio` | `en-US-Studio-…` | 160 USD | 0.25–4 | — |

  The card offers a few US English voices; `voice_name` takes any voice of the model's family in
  any language (`de-DE-Neural2-B`), and the language is read from the name. A voice of another
  family is refused before sending, so a run is never billed at another family's price than the
  one quoted. The text may be plain or SSML (`input_format`), at most 5,000 bytes. Google counts
  every character sent, SSML tags included, and so does the recorded cost. There is no quote
  before the run.
- **Gemini voices:** `google-tts.gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-3.1-flash`
  (preview) and `gemini-2.5-flash-lite` (preview). One of the 30 prebuilt voices (default Kore),
  a BCP-47 `language_code` (default `en-US`), and an optional `style` prompt ("Say this warmly,
  slowly"). Naming both `speaker_1` and `speaker_2` (letters and digits) with a voice each makes
  a dialogue: lines in the text start with a speaker's name and a colon. Flash-Lite speaks with
  one voice only. Text and style are at most 4,000 bytes each. Gemini-TTS is billed by tokens
  (for example 2.5 Flash at 0.50 USD per 1M input and 10 USD per 1M output tokens), and the
  reply reports none, so the catalog marks these `by_usage`: no price before the run, and none
  after it.
- **Flow:** everything is checked before the free token exchange; the synthesis request is the
  paid step and is never replayed. A 400, 401, 403, 404 or 429 that carries Google's matching
  error status is a refusal made before any speech, so it isn't billed; anything else (a 5xx, a
  lost reply, an unreadable body) may have been. A 400 quotes Google's explanation (a bad voice,
  language or SSML), unless it echoes the token. The audio must sniff as MP3 and stay under
  16 MiB. No redirects are followed. There is nothing to poll or cancel.
- **Not verified live:** whether Studio voices take a pitch (it isn't sent), and whether the
  preview Gemini model names stay as they are. The 5,000-byte classic limit is on Google's quotas
  page. Only the service account's own project is billed; no quota-project header is sent, so
  classic voices need no IAM role.
  Spending money needs authorization. With it, run `NODECRAFT_LIVE_GOOGLE_TTS=1
  NODECRAFT_LIVE_GOOGLE_TTS_ACKNOWLEDGE_COST=1 GOOGLE_TTS_KEY_FILE=/path/to/key.json cargo test
  -p nodecraft-providers --features http,google-tts --test live_google_tts -- --ignored`. It
  speaks one short Standard line and one Gemini 2.5 Flash line, well under 0.01 USD.

Contract checked 2026-10-23: [pricing](https://cloud.google.com/text-to-speech/pricing),
[`text:synthesize`](https://cloud.google.com/text-to-speech/docs/reference/rest/v1/text/synthesize),
[Gemini-TTS](https://docs.cloud.google.com/text-to-speech/docs/gemini-tts),
[Chirp 3: HD voices](https://cloud.google.com/text-to-speech/docs/chirp3-hd).

## Veo on Google Cloud (`google-veo` feature)

`Veo` runs Veo 3.1 on Vertex AI (#742). Since the Gemini API's Veo models were shut down, this is
how NodeCraft reaches Veo. It signs in as a Google Cloud **service account**:

- **The key.** Create a service account in a project with the Vertex AI API enabled, give it the
  *Vertex AI User* role, create a JSON key and paste the whole file as the `google-cloud` key. A
  key file is about 2.3 KB, more than a keychain entry holds everywhere. So only what signing
  needs is stored, as one line (`gsa1:<email>:<project>:<key id>:<p>:<q>`, the RSA key's primes in
  base64url). Files for other account types, other token endpoints or domain-scoped projects are
  refused without being quoted, as are RSA keys outside 2048–4096 bits. The `rsa` crate (pinned)
  does the signing.
- **Tokens.** The adapter signs a JWT (RS256, scope `cloud-platform`) and exchanges it at
  `https://oauth2.googleapis.com/token` for an access token. The token is held until five minutes
  before it expires, and the exchange is free. A 401 from Vertex AI drops the held token, so the
  next request gets a new one. A refused exchange is an `Auth` error naming the likely cause (a
  deleted key or a wrong clock).
- **Models:** `google-veo.veo-3.1` (`veo-3.1-generate-001`), `google-veo.veo-3.1-fast` and
  `google-veo.veo-3.1-lite`, run in the account's project at `us-central1`.
- **Modes:** all three make video from text. Veo 3.1 and Fast also take a first frame with an
  optional last frame, or one to three subject references (`referenceType: asset`, 8-second clips
  only). Images are PNG or JPEG, at most 20 MiB each and 60 MiB together, sent inline. Lite's image
  input isn't documented on Vertex AI, so Lite is text only.
- **Options:**
  - aspect ratio 16:9 (default) or 9:16
  - resolution 720p (default), 1080p, or 4K on Veo 3.1 only
  - 4, 6 or 8 seconds (default 8)
  - an optional seed

  Each run makes one clip, always with sound.
- **Flow:**
  - `POST …/publishers/google/models/{model}:predictLongRunning` is the paid step and is never
    replayed.
  - The returned operation name is the job's remote id. It is polled every 10 s with
    `POST …/{model}:fetchPredictOperation`. Only operation names of a known Veo model in `us-central1` are
    polled, and always at the account's own project.
  - The finished video comes inline (`bytesBase64Encoded`). It must sniff as MP4 and stay under
    128 MiB. A reply with only a Cloud Storage URI is refused, since no bucket is configured.
  - No redirects are followed.
  - A video removed by Google's safety filters is reported as blocked content with the filter
    categories, and so is an error carrying Google's support codes.
- **Cost:** quoted before the run, at the list price per second of video with audio:

  | Model | 720p | 1080p | 4K |
  |---|---|---|---|
  | Veo 3.1 | 0.40 USD | 0.40 USD | 0.60 USD |
  | Fast | 0.10 USD | 0.12 USD | — |
  | Lite | 0.05 USD | 0.08 USD | — |

  The catalog prices this with `per_unit_by_choice`: the duration counts the seconds and the
  resolution picks the price. Operations can't be cancelled.
- **Not verified live:**
  - whether the pricing page's "/1 count" means per second
  - whether 4K is generally available
  - whether a video removed by the safety filters is billed
  - what the model page's "pay-as-you-go isn't supported" means (perhaps that a quota request is
    needed)

  Spending money needs authorization. With it, run
  `NODECRAFT_LIVE_GOOGLE_VEO=1 NODECRAFT_LIVE_GOOGLE_VEO_ACKNOWLEDGE_COST=1
  GOOGLE_VEO_KEY_FILE=/path/to/key.json cargo test -p nodecraft-providers --features
  http,google-veo --test live_google_veo -- --ignored`. It makes one 4-second 720p Fast clip, about
  0.40 USD.

Contract checked 2026-10-22: [Veo 3.1 model](https://cloud.google.com/vertex-ai/generative-ai/docs/models/veo/3-1-generate),
[video generation API](https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/veo-video-generation),
[pricing](https://cloud.google.com/gemini-enterprise-agent-platform/generative-ai/pricing) (section "Veo"),
[service-account JWT flow](https://developers.google.com/identity/protocols/oauth2/service-account).

## Gemini images on Google Cloud (`google-gemini-images` feature)

`GoogleGeminiImages` runs Google's Gemini image models on Vertex AI (#47), signed in with the same
`google-cloud` service-account key and tokens as Veo (see above). Imagen 4, the issue's original
target, is discontinued on Vertex AI, so this adapter covers the image models that replace it.

- **Models:** `google-gemini-images.3.1-flash` (`gemini-3.1-flash-image`), `3-pro`
  (`gemini-3-pro-image`), `nano-banana-2.1` (`gemini-nano-banana-2.1`), `3.1-flash-lite`
  (`gemini-3.1-flash-lite-image`) and `2.5-flash` (`gemini-2.5-flash-image`, deprecated). They run
  in the account's project on the `global` endpoint, the only one the Gemini 3 image models are
  served on.
- **Modes:** text → image, and a prompt with reference images → image (`edit`): up to 14 images,
  3 on 2.5 Flash. Images are PNG, JPEG or WebP, at most 7 MB each and 48 MiB together, sent
  inline; one whose bytes don't match its type is refused before anything is sent.
- **Options:** aspect ratio (1:1 by default; the ten standard ratios, plus 1:4, 4:1, 1:8 and 8:1
  on 3.1 Flash, the only model documented with them) and, on 3.1 Flash, 3 Pro and Nano Banana 2.1, image size 1K (default), 2K or 4K.
  Flash-Lite makes 1K only and 2.5 Flash has one size. 3.1 Flash's 512 size isn't offered:
  Vertex AI's `imageConfig` documents only 1K, 2K and 4K.
- **Flow:** one `POST …/locations/global/publishers/google/models/{model}:generateContent` with
  `responseModalities` TEXT and IMAGE and `candidateCount` 1. It is the paid step and is never
  replayed after a lost reply; there is nothing to poll or cancel. Images come back inline. Each
  must decode to at most 32 MiB (64 MiB together, 96 MiB for the whole reply) and sniff as the PNG,
  JPEG or WebP it claims to be, with a header of at most 16,384 pixels a side and 64 Mi pixels;
  thinking parts are skipped. A reply may hold more than one image,
  and every one becomes an output.
- **Refusals and filters:**
  - a blocked prompt (`promptFeedback.blockReason`) or a reply whose images were removed by a
    safety, recitation, blocklist or Model Armor filter is reported as blocked content, with
    Google's code
  - a reply with no image and no filter reason is a failed job, quoting the model's text
  - a 401 drops the held token; 403 names the Vertex AI API and the Vertex AI User role; 429
    keeps Google's `Retry-After`
- **Cost:** quoted before the run as one image at the chosen size, from the pricing page's
  tokens per image at the model's image output rate:

  | Model | 1K | 2K | 4K |
  |---|---|---|---|
  | 3.1 Flash Image | 0.0672 USD | 0.1008 USD | 0.1512 USD |
  | 3 Pro Image | 0.1344 USD | 0.1344 USD | 0.24 USD |
  | Nano Banana 2.1 | 0.0336 USD | 0.0504 USD | 0.1134 USD |
  | 3.1 Flash-Lite Image | 0.0336 USD | — | — |
  | 2.5 Flash Image | 0.0387 USD (one size) | — | — |

  The catalog prices this with `per_unit_by_choice` and a fixed count of one image. A finished run
  is charged what the reply measured (`usageMetadata`): prompt tokens, reference images included,
  at the input rate, and reply tokens at the image output rate. That covers reference images and
  replies with several images. The reply's few text tokens are charged at the image rate (a little
  over), and thinking tokens, which Google bills at the text rate, aren't charged (a little under).
  The quote leaves out the prompt and reference images, so an edit run is charged more than it
  was quoted: with 14 references, about 12% more on 3 Pro or 3.1 Flash. A budget that admits the
  quote can be passed by that much.
- **Not verified live:**
  - whether a reply whose images were filtered is billed (it is treated as possibly billed, so it
    is not marked refused)
  - `imageConfig` is marked deprecated in favour of a newer response-format field; the documented
    `imageConfig` is what is sent

  Spending money needs authorization. With it, run
  `NODECRAFT_LIVE_GOOGLE_GEMINI_IMAGES=1 NODECRAFT_LIVE_GOOGLE_GEMINI_IMAGES_ACKNOWLEDGE_COST=1
  GOOGLE_GEMINI_IMAGES_KEY_FILE=/path/to/key.json cargo test -p nodecraft-providers --features
  http,google-gemini-images --test live_google_gemini_images -- --ignored`. It makes one 1K
  Flash-Lite image, about 0.034 USD.

Contract checked 2026-10-23: [3.1 Flash Image model](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-1-flash-image),
[image generation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/capabilities/image-generation),
[`generateContent`](https://cloud.google.com/vertex-ai/docs/reference/rest/v1/projects.locations.publishers.models/generateContent),
[pricing](https://cloud.google.com/gemini-enterprise-agent-platform/generative-ai/pricing).

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
interval and retry/backoff policy. Creation sends `Cancel-After` using the host's
`RunLimits::deadline_ms` duration, rounded upward to whole seconds. Durations outside 5 seconds
to 24 hours are rejected before sending a prediction. Direct `Provider::submit` uses the usual
one-hour default. This is a server lifetime measured from prediction creation, whereas the local
deadline begins when polling starts; they are separate clocks. Neither is proof of nonbilling.
Polling, restored handles, output downloads and cancellation never reset the server lifetime.
The local deadline still requests best-effort cancellation when a handle exists.

On successful completion, `metrics.predict_time` becomes optional `Usage.execution_ms`, rounded
to milliseconds. It measures execution rather than queue time, tokens or money. Missing or
malformed optional metrics remain unknown without discarding valid outputs; negative, nonfinite
or over-24-hour values are not retained. Failed/canceled predictions still report their failure,
without a completed-result usage record. Cost stays unknown. The engine retains this evidence
in result job settings, held results and resumed jobs; copying a cached result does not claim
another execution. Older records without it remain readable.

Contract checked 2026-10-24: [prediction deadlines](https://replicate.com/docs/topics/predictions/create-a-prediction)
and [HTTP API metrics](https://replicate.com/docs/reference/http).


## Gemini Images (`gemini-images` feature)

The existing host provider factory reads the `gemini` keychain account for `gemini-images`,
using the same readiness and routing path as other catalog adapters. Construction sends no
requests. The shipping RunHost resolves wired image assets for catalog editing.

GeminiImages uses the stateless Gemini Interactions REST API at the fixed Google origin, with store/background/stream false. Supported stable model IDs are gemini-nano-banana-2.1, gemini-3.1-flash-lite-image and gemini-3-pro-image. Vertex and retired Imagen endpoints are not implemented. The catalog offers generation and editing with up to fourteen ordered PNG/JPEG/WebP references. Lite is not optimized for multiple-reference or sequential editing. No masks, remote URLs, conversation state, search/tools, count, quality or output-format switches. Output is explicitly inline JPEG. Lite's wire image_size is `1K` per REST enum and guide, despite its model page's conflicting `1024px` prose. Other models offer 1K/2K/4K; the documented local ten-aspect subset is in the catalog.

Pass an API key as a Secret from the host's OS credential store, not a project field. The adapter uses x-goog-api-key as a secret header, generic User-Agent, no redirect following, and no automatic replay of uncertain paid requests. The local idempotency key is not a remote deduplication guarantee. A timeout, redirect, malformed response or cancellation after dispatch may still be billed. Poll has no resumable handle and cancel cannot confirm remote cancellation. Builtin browser transport explicitly refuses no-redirect requests; wasm compilation is supported with an audited host transport. No keys, prompts or provider response excerpts appear in adapter diagnostics.

Pricing is ByUsage USD, unknown rather than free; token counts do not become actual currency. Every finite budget cap, including zero, refuses an unknown paid registration before transport. An explicitly uncapped host may run. Missing or malformed optional usage never discards otherwise valid images; returned Usage.cost is None. Main can have no numeric ledger record when neither estimate nor actual cost is available; this provider does not repair that limitation or imply a zero bill.

The adapter preserves all final image/text blocks in order and validates the complete image collection before returning Completed. Unknown output types, URI-only output, corrupt later images and incomplete/nonterminal responses refuse rather than silently publish partial results. Thought summaries are bounded private metadata, not final output. Text-only output reports an actionable malformed/no-image result; the selected Interactions schema does not expose generateContent finishReason/safetyRatings, so arbitrary 403 or a text sentence is not guessed to be BlockedContent.

The shipping RunHost uses the session asset store to resolve graph Media assets through `queued_job_with_media`. Offline tests wire a stored reference into each of the Gemini, Seedream and OpenAI Images catalog cards, dispatch `run.quote`/`run.start`, and record the real adapter's complete result. A standalone `generate::provider_job` call without a media source still refuses media inputs; custom hosts must supply `provider_job_with_media` and preserve completed results when conversion refuses. These local fixtures establish wiring and contracts, not paid service availability or output quality.

Local lower-only limits: prompt 128,000 bytes / 32,000 characters; 15 inputs / 14 references; 8 MiB per input and 48 MiB total; 64 MiB request; 65 MiB response; 16 final images; 16 MiB per output and 48 MiB total. Structural limits are 64 steps / 256 content items, JSON depth 16 / 8,192 members / 16,384 values / 4,096 strings, 256 KiB metadata including 64 KiB text, and 64 MiB aggregate image base64. Images have an 8,192-pixel axis cap, 18,000,000 pixels per image and 36,000,000 pixels total. These shared image limits apply to references as well as outputs. The aggregate admits two largest documented 4K outputs with modest pixel headroom; every byte, count and pixel bound still applies. Any later invalid or excessive image refuses the whole collection. An original detailed 4800×3584 JPEG exceeding the old 8 MiB cap is retained byte for byte under the new default.

For Nano and Pro, requested 1K/2K/4K geometry is checked against lowered axis, per-image, two-image aggregate and JPEG pixel/orientation allowances before reference decoding or POST. This cannot predict the actual returned count or encoded byte length. Lite has no separately identified dimension table in the checked guide: its default envelope is supported, but lowering any geometry or codec allowance conservatively refuses before POST. Actual probe and full codec validation remain mandatory for every image; the 256 MiB codec setting reserves pixels and possible orientation copies, not all codec workspace.

The synchronous request allowance is 300 seconds, providing local service/body-transfer headroom for expensive work without automatically replaying it. This is not a measured service SLA, the engine's total RunLimits deadline, or proof that timeout/cancellation avoids billing. Catalog latency remains an approximate 120-second hint. The fixed request encoder uses a 4,096-byte stack buffer. The [updated phase worksheet](decisions/2026-10-22-paid-image-output-admission.md#resource-ownership) charges known JPEG input/metadata copies and keeps every phase below the existing 628 MiB logical envelope; it does not bound allocator capacities, codec-private workspace, transport internals, concurrent host owners or RSS.

Original synthetic fixtures run offline. The ignored native smoke requires NODECRAFT_LIVE_GEMINI_IMAGES=1 plus NODECRAFT_LIVE_GEMINI_IMAGES_ACKNOWLEDGE_COST=1 and explicit spending authorization; optional second paid edit additionally requires NODECRAFT_LIVE_GEMINI_IMAGES_EDIT=1. No live test is selected in CI. Current source has not been validated against a paid service call.

Contract checked 2026-10-18: [REST](https://ai.google.dev/api/interactions-api), [image guide](https://ai.google.dev/gemini-api/docs/image-generation), [Lite](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite-image), [Nano](https://ai.google.dev/gemini-api/docs/models/gemini-nano-banana-2.1), [Pro](https://ai.google.dev/gemini-api/docs/models/gemini-3-pro-image).
## Gemini Video (`gemini-video` feature)

`GeminiVideo` runs Gemini Omni Flash (`gemini-omni-1.1-flash`), Google's video model on the Gemini
API now that every Veo model there is shut down (Veo 3.1 on 2026-10-22). It uses the `gemini`
keychain account, like Gemini Images. Catalog model `gemini-video.omni-flash`:

- **Modes:** text to video; a first frame with an optional last frame; one to three subject
  references. Images are PNG or JPEG, at most 15 MiB each and 45 MiB together, sent inline. The
  prompt is required; there is no negative prompt, seed or duration (ask for timing in the
  prompt, e.g. `[0-3s] …`). The video comes with generated sound.
- **Options:** aspect ratio 16:9 (default) or 9:16; resolution 360p, 720p (default) or 1080p
  (upscaled). 4K, editing and extending a video, and video references are not offered yet.
- **Flow:** one synchronous `POST /v1beta/interactions` (store, background and stream false,
  `delivery: "uri"`; allowed up to 15 minutes) is the paid step and is never replayed. The reply
  names a Files API file, which is polled every 5 s with `GET /v1beta/files/{id}` until `ACTIVE`
  and downloaded from `files/{id}:download?alt=media` (redirects followed; the key is dropped on
  leaving Google's API origin). The video must sniff as MP4 and stay under 256 MiB. A reply that
  carries the video inline finishes at submit. Files expire on Google's side after two days and
  are not deleted by NodeCraft.
- **Cost:** no quote before the run: the clip's length is the model's choice. The result is
  charged by measured tokens at 1.50 USD per million input and 17.50 USD per million output
  tokens (about 0.10 USD a second of 720p). Finite budgets refuse by-usage models.
- **Not verified live:** whether a reply ever carries the video inline despite `uri` delivery,
  and the exact form of the file reference (both `files/<id>` and the full API URL are read). Run
  `NODECRAFT_LIVE_GEMINI_VIDEO=1 NODECRAFT_LIVE_GEMINI_VIDEO_ACKNOWLEDGE_COST=1 GEMINI_API_KEY=…
  cargo test -p nodecraft-providers --features http,gemini-video --test live_gemini_video --
  --ignored` with authorization to spend.

Contract checked 2026-10-22: [Omni guide](https://ai.google.dev/gemini-api/docs/omni),
[pricing](https://ai.google.dev/gemini-api/docs/pricing),
[deprecations](https://ai.google.dev/gemini-api/docs/deprecations).

## Kling (`kling` feature)

`Kling` (#19) drives Kling's task API at the fixed base `https://api-singapore.klingai.com`. Four
catalog models share it:

| Catalog model | Kling model | Modes | Options |
|---|---|---|---|
| `kling.v2_5_turbo` | `kling-v2-5-turbo` | text → video; start frame, optional end frame → video | 5 or 10 s, Standard or Professional, aspect ratio (text only) |
| `kling.v2_1_master` | `kling-v2-1-master` | text → video; start frame → video | 5 or 10 s, aspect ratio (text only) |
| `kling.v1_6` | `kling-v1-6` | text → video; start frame, optional end frame → video; 1–4 references → video | 5 or 10 s, Standard or Professional, prompt adherence 0–1, aspect ratio |
| `kling.image_v2_1` | `kling-v2-1` | text → one image | eight aspect ratios |

- **Key.** Store the access key and secret key joined by a colon (`ak:sk`), as Kling's developer
  console issues them; a single API key without a colon is sent as a bearer token as is. The pair signs an HS256 token for each request (issuer the access
  key, valid from five seconds ago for 30 minutes); the secret key never travels. A key with more
  than one colon, or an empty part, is refused before anything is sent.
- **Inputs.** Prompts and negative prompts are at most 2,500 characters. Images are PNG or JPEG
  whose bytes match their type, at most 10 MiB each and 40 MiB together, sent inline as base64. An
  end frame needs a start frame and Professional mode; references and frames don't mix. A start
  frame sets the clip's shape, so the aspect ratio isn't sent with one.
- **Flow.** One `POST /v1/videos/text2video`, `/v1/videos/image2video`,
  `/v1/videos/multi-image2video` or `/v1/images/generations` creates the task: the paid step,
  never replayed. Video creates carry `external_task_id`, a BLAKE3 hash of the job's idempotency key,
  so Kling refuses a second task for the same job. The task is polled every 5 s at the same path
  plus its id; the result link is downloaded at once without the key (no redirects), must sniff as
  MP4 (video) or PNG, JPEG or WebP (image) and stay under 512 MiB. A failed download is retried on
  the next poll.
- **Errors.** Kling's business codes map to what a person can do: 1000–1004, 1100, 1103 and
  1304 a key or account problem, 1101/1102 the account balance, 1200–1202 the request, 1203 a
  task Kling doesn't know, 1300/1301 content policy, 1302 rate limits, 1303 and 5001 busy, 5000 a
  server error, 5002 a timeout. A message that echoes the key or a signed token is withheld. Such
  a refusal of the create proves no task was made, unless it names an existing task; a server
  error or lost reply after a create says the task may have been billed.
- **Cost.** Kling bills prepaid resource units by model, mode and length, so the catalog lists
  every model as by usage: no price before the run, and finite budgets refuse them.
- **Cancel.** Kling has no cancel endpoint; a task runs to the end and is billed.
- **Not verified live.** The official docs refuse scripted reads; the contract follows the
  public mirrors linked below. Open: whether Kling 2.5 Turbo text to video takes Standard mode
  (the default), whether a plain API key is accepted, and the code for a repeated
  `external_task_id`. Run `NODECRAFT_LIVE_KLING=1 NODECRAFT_LIVE_KLING_ACKNOWLEDGE_COST=1
  KLING_API_KEY=… cargo test -p nodecraft-providers --features http,kling --test live_kling --
  --ignored` with authorization to spend.

Contract checked 2026-10-23: [Kling developer docs](https://app.klingai.com/global/dev/document-api),
[AI SDK Kling provider](https://ai-sdk.dev/providers/ai-sdk-providers/klingai).

## fal.ai queue models

Ideogram V3 (#34) reuses this adapter and the `fal` keychain account: select
**Ideogram V3 Balanced (fal)** on an Image card. `fal.ideogram-v3` sends one image request,
fixed to Balanced rendering, with a prompt, optional negative prompt, six size presets,
four styles, MagicPrompt and an optional seed (0–2,147,483,647). The dated estimate is
0.06 USD per call, checked on 2026-10-27 against [fal pricing](https://fal.ai/models/fal-ai/ideogram/v3)
and its [HTTP schema](https://fal.ai/models/fal-ai/ideogram/v3/api). The alias is live;
the estimate is separate from any observed invoice. Other rendering speeds, multiple images,
reference/edit inputs and custom palettes are not exposed by this entry.

Ideogram uses the existing queue resume/cancel flow and no-redirect authentication. Its
generic file result may omit dimensions; downloaded PNG/JPEG/WebP bytes are probed and
decoded within the existing 16 MiB and 4,096-pixel bounds. No incomplete image is imported.
The opt-in ignored smoke test needs `NODECRAFT_LIVE_IDEOGRAM=1` and `FAL_KEY` and spends
one Balanced call; offline fixtures require neither. Direct Ideogram credentials are not used.

The optional `fal` feature supports explicit live aliases, checked against official HTTP
schemas on 2026-10-18 (the two Wan image and speech aliases and the two style models on
2026-10-21, Bria on 2026-10-22): `fal-ai/flux/schnell`, `fal-ai/wan/v2.2-a14b/text-to-video`,
`fal-ai/wan/v2.2-a14b/image-to-video`, `fal-ai/wan/v2.2-14b/speech-to-video`,
`fal-ai/kokoro/american-english`, `fal-ai/trellis-2`, `fal-ai/esrgan`,
`fal-ai/flux-lora-fast-training`, `fal-ai/flux-lora` and `fal-ai/bria/background/remove`. These names are not
immutable weight versions; upstream schema/model availability can change. Hosts inject a Secret
and HTTP transport. The adapter never reads credentials, project keys or live-test variables.
The existing host provider factory reads the `fal` keychain account and constructs the adapter
off the UI thread, following the same readiness/routing path as other catalog providers.
The offline Bria fixture below exercises the shipping RunHost and stored-image resolver with this adapter; it does not call fal.

FLUX requests 512–1024 dimensions in multiples of 64, one to four PNG outputs, one to four steps
and optional u32 seed. Wan selects 480p/16fps, 17–81 frames and three aspect ratios, explicitly
turning interpolation off. Wan image to video takes a `first_frame` (`image_url`) and an
optional `last_frame` (`end_image_url`), 17–161 frames, 480p/580p/720p and an aspect ratio that
defaults to following the frame, again without interpolation. Wan speech to video takes a
`portrait` (`image_url`) and up to 7.5 s of `speech` (`audio_url`; its header must sniff as one
of NodeCraft's accepted WAV, MP3, Ogg or M4A and match its declared type), 40–120 frames in multiples of 4 and the same resolutions; whether its MP4 carries
the speech track is not yet verified by a live run. Inputs are 1 MiB and 1024 px each; a request
is at most 4 MiB after base64. A clip from any Wan model declared `application/octet-stream`
counts as undeclared and is still sniffed as MP4 (#607). Kokoro selects af_heart/am_adam and speed 0.5–2.0, returning WAV.
Trellis selects resolution 512 and one inline image, returning GLB2. ESRGAN selects scale2,
RealESRGAN_x4plus, face=false and PNG. Advanced/unknown parameters and extra roles refuse
before HTTP. Image-to-3D/upscale input contracts are direct-adapter capabilities; their engine
fixtures use an explicitly test-only immutable asset resolver. Those older fixtures do not prove
shipping input resolution; the separate Bria fixture exercises the shipping resolver.

Bria background removal (#749) exposes `fal.bria-rmbg` with mode `remove-background`,
one required `image` input and one PNG output. Its image-only estimate is 0.018 USD per call,
checked 2026-10-22 against [official pricing](https://fal.ai/models/fal-ai/bria/background/remove)
and the [HTTP schema](https://fal.ai/models/fal-ai/bria/background/remove/api). The request
contains only `image_url` and `sync_mode=false`. Prompts are refused: the provider prices
guided removal separately at 0.03 USD. The existing 1 MiB / 1024 px input limit applies,
and the expected output geometry is checked against configured limits before paid dispatch.
The result must be one complete PNG with the input dimensions; transparent pixels are retained.
Inputs requiring an EXIF rotation or reflection are refused before codecs, encoding or paid
dispatch: apply the orientation and save the upright image first. The service's EXIF policy is
unverified. The first opt-in live check must confirm that this endpoint preserves input dimensions
without resizing or padding; the exact-size rule is a conservative offline contract.
Offline fixtures cover the catalog-generated card, the 0.018 USD quote and explicit consent,
the real adapter behind RunHost, stored input resolution and importing the distinct PNG result.
No live availability or invoice is claimed. The dedicated Image Tool menu is tracked separately
by #740; this addition supplies the catalog mode and execution path without depending on that PR.

The catalog uses ByUsage USD for FLUX, the Wan models, Kokoro and ESRGAN: unknown is not
zero/free. Every finite budget cap refuses an unknown paid quote before invocation; an uncapped
host still needs actual
run.quote/run.start partial-price consent. Trellis512 has a dated 0.25 USD per-call estimate,
verified from the [official endpoint](https://fal.ai/models/fal-ai/trellis-2/api?platform=http).
Absent monetary usage never becomes a provider-observed invoice: existing runner estimate
fallback is separate. Unknown-price completions may have no numeric money row, not free work.

Trained styles (#167): `fal-ai/flux-lora-fast-training` takes 4 to 12 `images` (each like any
fal input image, 12 MiB together), packs them uncompressed into one zip sent as an
`images_data_url` data URI (a request is at most 17 MiB), and sends `steps` (1–5000),
`is_style=true`, `create_masks=false` and the optional one-line `trigger_word`, which the job
handle keeps. Its result is never downloaded: `diffusers_lora_file.url` must be a plain `https`
link on `fal.media`, and the adapter returns a style descriptor naming it (provider `fal`, family
`flux-1`). `fal-ai/flux-lora` takes a `prompt` and one `style`, sends it as
`loras=[{path, scale}]` with `lora_scale` 0–2, `guidance_scale` 1–20 and 1–50 steps, and puts the
trigger word before a prompt that doesn't already contain it. It accepts only descriptors fal's
trainer made for `flux-1` whose weights are on `fal.media`; any other is refused before the
request. The trainer is priced 2 USD per 1000 steps (dated 2026-10-21); FLUX with a style is
by usage (fal prices it per megapixel). Two things are not yet verified by a live run: whether fal accepts a data-URI zip
for training, and whether every trained weights link is a plain `fal.media` link. A link on
another host or with a query is refused after the training was paid for, and nothing is kept.

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
ESRGAN their complete model names, and Bria fal-ai/bria. Request IDs are one ASCII alphanumeric/hyphen/underscore
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
4096 items. Four output files maximum, 16 MiB each / 48 MiB total, except one Luma Ray clip may use
256 MiB for both the file and total download; image axes 4096 and aggregate
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
| Image download validation | Validation 2 + metadata 1 + API reply 2 + outputs 48 + Vec-to-Arc live-byte overlap 16 + combined rasters 128 + scratch 1 | 198 |

A separate container/admin payload allowance of 4 gives a 202 MiB image-path logical-payload worksheet,
not an allocation or RSS ceiling. HTTP body **length** is bounded; transport `Vec` capacity may
exceed that length through allocator growth and is additional, including its overlap with Arc
conversion. The adapter must not reject already-paid valid bytes merely because capacity grew.
Pixel/orientation objects drop before the next codec/encoder phase; 16-bit PNG and orientation
are included in known raster owners. Existing media preflight uses decoder.total_bytes plus an
orientation copy against 128 MiB; most codecs ignore their private allocation limit.
Codec-private workspace, spare transport capacity, allocator overhead, transport/TLS/kernel
buffering, caller graph/history and concurrent jobs are outside this worksheet.

Ray instead admits one MP4 body up to 256 MiB and a possible 256 MiB Vec-to-Arc live-byte
overlap. Its container-envelope check does not allocate image codec rasters. These payloads
replace the image download/codec owners above; the 202 MiB image worksheet is not a Ray cap.

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

Response admission deliberately requires the exact dated model ID and exactly one complete JPEG
with no bytes after EOI. The response `size` field is advisory: missing, malformed, duplicated or
mismatched metadata does not discard an otherwise valid paid JPEG. Geometry, aspect ratio and
pixel limits are checked against the actual JPEG bytes before full decoding. Model aliases,
corrupt images or trailing data still refuse; such refusals after sending may still be billed.
Malformed optional usage preserves an otherwise valid image
with unknown usage; token counts and catalog estimates never become fabricated actual charges.
The [logical memory worksheet](decisions/2026-10-18-seedream-local-delivery-and-resource-limits.md)
records phase owners and exclusions; codec workspace and process RSS are not universally bounded.

All HTTP uses the explicit no-redirect contract, with no fallback. Browser Reqwest refuses it, so
wasm needs an audited host transport. The adapter does not follow/download result URLs. The
shipping RunHost resolves wired assets for catalog editing; the offline real-adapter regression
records a complete JPEG even when the response omits advisory size. Custom hosts retain Completed
if output routing/conversion refuses it.

The ignored live test requires NODECRAFT_LIVE_SEEDREAM=1,
NODECRAFT_LIVE_SEEDREAM_ACKNOWLEDGE_COST=1 and ARK_API_KEY, plus separate explicit human spending
authorization. It never runs in CI; ordinary contracts use authored fixtures and local HTTP.

## Seedance video tasks

The optional `seedance` adapter (feature `seedance`, #18) creates video tasks on BytePlus
ModelArk at the fixed endpoint
`https://ark.ap-southeast.bytepluses.com/api/v3/contents/generations/tasks`, with the same API
key as Seedream. Four dated models appear on the Generate video card:

| Catalog model | Model | Length | Resolutions | References | Quote per second (720p) |
|---|---|---|---|---|---|
| `seedance.2-5` | `dreamina-seedance-2-5-260628` | 4–30 s | 480p, 720p, 1080p | up to 30 | 0.232577 USD |
| `seedance.2-0` | `dreamina-seedance-2-0-260128` | 4–15 s | 480p, 720p, 1080p, 4K | up to 9 | 0.152153 USD |
| `seedance.2-0-fast` | `dreamina-seedance-2-0-fast-260128` | 4–15 s | 480p, 720p | up to 9 | 0.121723 USD |
| `seedance.2-0-mini` | `dreamina-seedance-2-0-mini-260615` | 4–15 s | 480p, 720p | up to 9 | 0.076077 USD |

Each model takes a prompt (up to 8,000 characters) and either a first frame, optionally with a
last frame, or reference images; never frames and references together. Images are PNG, JPEG or
WebP whose bytes match their type, under 30 MB each and 45 MiB together, 300 to 6,000 pixels a
side and at most 2.5 times as wide as tall (or tall as wide). They are sent inline as data URLs.
The settings are resolution (default 720p), aspect ratio (`adaptive`, the default, or one of
16:9, 4:3, 1:1, 3:4, 9:16, 21:9), duration (default 5 s), sound (default on) and an optional seed
(0–2,147,483,647). Every one is sent explicitly, with `watermark` off, so nothing is left to the
model's defaults. Seedance 2.5 keeps a first frame's own shape, so with a frame it needs the
adaptive ratio. Anything else is refused before a request is sent.

**Price.** BytePlus bills a finished video by its tokens: width × height × 24 × seconds / 1024,
at a rate per million tokens (pricing page checked 2026-10-23: 2.5 at 10.7 USD, 11.7 at 1080p;
2.0 at 7.0, 7.7 at 1080p and 4.0 at 4K; Fast 5.6; Mini 3.5). The quote is that formula at each
resolution's largest listed output size, so it is an upper bound per second: a square clip has
fewer pixels and costs less. An adaptive clip follows its frame or the prompt, and the quote
assumes it is no larger than the listed sizes. The finished task's `completion_tokens` are
recorded with the result as evidence; the recorded cost is the quote. A failed task isn't billed.

**Submission.** Creating the task is the paid step. ModelArk has no idempotency key, so a create
is never replayed automatically. A reply at one of the statuses ModelArk's error table documents
for a refused request (400, 401, 403, 404 or 429) with an error code proves no task was created
and releases the reservation; a 5xx, a lost reply or an unreadable body may hide a created task,
and the error says it may be billed when it succeeds. Error codes map onto the shared kinds:
content-safety codes (`…SensitiveContentDetected…`) to blocked content, authentication and
activation codes (including `ModelNotOpen`) to auth, overdue accounts and a spending limit
(`SetLimitExceeded`) to budget, `ServerOverloaded` to busy. Messages are cut short and withheld if
they echo the key.

**Polling and results.** The task is polled every five seconds. A finished task names a
pre-signed link valid for 24 hours, which is downloaded **without the key** from public `https`
hosts only; the download must be an MP4 of at most 512 MiB. A failed download is retryable: the
next poll reads the link again, so a billed video is never given up on one failed fetch.

**Cancelling.** ModelArk cancels a queued task when it is deleted, but deleting a finished task
erases its record and with it a billed video's link, and a running task can't be stopped. So the
adapter reads the task first and deletes it only while it is queued; a running task is left to
finish (and is billed only if it succeeds). A task that starts between the read and the delete is
refused by ModelArk and keeps running.

Not offered: video and audio references (ModelArk takes them only as public links), draft mode
and the Seedance 1.x models. Seedance is native only: results are fetched with no-redirect
requests, which the builtin browser transport refuses.

**Not verified live yet:** the price formula's output sizes for `adaptive` clips, and that the
listed rates apply to the API as billed.

The ignored live test requires `NODECRAFT_LIVE_SEEDANCE=1`,
`NODECRAFT_LIVE_SEEDANCE_ACKNOWLEDGE_COST=1` and `ARK_API_KEY`, plus separate explicit human
spending authorization (Seedance 2.0 Mini, 480p, 4 s: at most about 0.15 USD). It never runs in CI:

```sh
NODECRAFT_LIVE_SEEDANCE=1 NODECRAFT_LIVE_SEEDANCE_ACKNOWLEDGE_COST=1 ARK_API_KEY=… \
  cargo test -p nodecraft-providers --features http,seedance --test live_seedance -- --ignored
```

## Hunyuan3D through fal

Issue #42 reuses the `fal` runtime and stored fal key for Tencent Hunyuan3D V3 cloud models.
Pick a **Hunyuan3D V3** model on a **Generate 3D** card. Tencent SecretId/SecretKey credentials
do not authenticate to fal. Native HTTP supports these requests; the builtin browser transport
refuses the required no-redirect operation before fetch. The library and catalog build for
WebAssembly and can use an audited host transport that supplies that guarantee.

Public contracts and prices checked **2026-10-27**:

| Model | Contract | Required inputs |
| --- | --- | --- |
| `fal.hunyuan3d-v3` | [Text to 3D](https://fal.ai/models/fal-ai/hunyuan3d-v3/text-to-3d/api) | One `prompt`, up to 1024 characters |
| `fal.hunyuan3d-v3-image` | [Photo to 3D](https://fal.ai/models/fal-ai/hunyuan3d-v3/image-to-3d/api) | One `image`; no prompt |
| `fal.hunyuan3d-v3-sketch` | [Sketch to 3D](https://fal.ai/models/fal-ai/hunyuan3d-v3/sketch-to-3d/api) | One `image` interpreted as a sketch, and a `prompt` up to 1024 characters |

All three return one GLB. Photo and sketch inputs are verified PNG/JPEG/WebP, at most 1 MiB
and 1024 pixels per axis, sent inline only after the complete escaped/base64 request fits the
4 MiB cap. Sketches require at least 128 pixels on each axis, checked before any paid request.
Empty/duplicate roles, unsupported fields and malformed images refuse before a paid
request. Safety stays on, and the provider's standard **500K face target** is retained without
requesting a custom count.

The mesh preset includes every offered price-changing setting. Current fal rates for
[text](https://fal.ai/models/fal-ai/hunyuan3d-v3/text-to-3d),
[photo](https://fal.ai/models/fal-ai/hunyuan3d-v3/image-to-3d), and
[sketch](https://fal.ai/models/fal-ai/hunyuan3d-v3/sketch-to-3d) generation are **$0.375 Normal**,
**$0.45 LowPoly**, **$0.225 Geometry**, plus **$0.15 for PBR**. Text/photo models offer textured,
PBR, low-poly triangles/quads with or without PBR, and geometry-only presets. Sketch offers
textured/PBR only: its API has no generation-type or polygon-type inputs. These are per-call
estimates, not provider-reported invoices. Custom face counts and multi-view images, which
add further fees, are outside this integration and are never silently sent or ignored.

Saved handles resume the same fal task without paying for another create. Ambiguous creates
never replay; terminal failures retain possibly billed reservations. Cancellation does not
promise a refund. Result downloads use fal storage without the key and never follow redirects.
Only `model_glb` is consumed; unrequested thumbnails and alternate-format links are not fetched.

Each GLB has a **256 MiB per-file/aggregate cap**, also on resume, so the documented 60+ MiB
examples fit. Existing packed-GLB checks refuse external resource URIs. Mesh decoding keeps its
256 MiB input, four-million vertex/triangle, depth and texture limits. Other fal aliases retain
their earlier quotas. A returned model using an unsupported decoder extension can remain in
history while its viewer reports that it cannot be read.

Offline contract, RunHost and GPU picker tests cover prices, typed/viewable GLB outputs,
failure accounting, saved tasks and a valid result larger than the old cap. Live provider
output has not been verified. The ignored `hunyuan42_live_smoke` needs
`NODECRAFT_LIVE_HUNYUAN=1`, `NODECRAFT_LIVE_HUNYUAN_ACKNOWLEDGE_COST=1` and `FAL_KEY`, plus explicit
spending authorization; it requests one Geometry model for about **$0.225** and never runs in CI.

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
there is no idempotency key, so a submit is never retried automatically. The exceptions are the
refusals in Tripo's error table that happen before a task exists, each at its documented status:
401 `1002`, 400 `1003`/`1004`, 429 `1007`/`2000` and 403 `2010` (too little credit "to create this
task"). They release the reservation, and the rate limits are retried with back-off (#631).
Polling is every three seconds and reports Tripo's queue position and percentage. Tripo has no
cancel endpoint, so cancelling a run stops waiting but not the task.

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

## Higgsfield requests

The optional `higgsfield` adapter (#177) drives the
[Higgsfield platform API](https://docs.higgsfield.ai/docs) at the fixed base
`https://api.higgsfield.ai`. Six catalog models share it:

| Catalog model | Endpoint | Inputs | Settings |
|---|---|---|---|
| `higgsfield.soul-2` | `higgsfield-ai/soul/v2/standard` | a prompt | aspect ratio (seven), 720p or 1080p, 1 or 4 images, prompt enhancement, an optional seed (1–1,000,000) |
| `higgsfield.kling-2.5-turbo-i2v` | `kling-video/v2.5-turbo/pro/image-to-video` | a prompt, an optional negative prompt, one image | 5 or 10 seconds, prompt adherence 0–1 |
| `higgsfield.genjutsu-motion-transfer` | `higgsfield/genjutsu/motion-transfer/v1.0` | an optional prompt, one MP4 source, 1–8 ordered reference images | 480p, 720p or 1080p |
| `higgsfield.genjutsu-object-swap` | `higgsfield/genjutsu/object-swap/v1.0` | an optional prompt, one MP4 source, 1–8 ordered reference images | 480p, 720p or 1080p |
| `higgsfield.genjutsu-restyle` | `higgsfield/genjutsu/restyle/v1.0` | an optional prompt, one MP4 source, 0–5 ordered reference images | a preset, 480p, 720p or 1080p |
| `higgsfield.soul-id-trainer` | `POST /v1/custom-references` (a Train style card) | 1–100 ordered pictures | a name |

Every setting is sent explicitly, defaults included, so a request means the same thing if
Higgsfield's defaults change. Higgsfield's key is two parts; store them in the keychain as one
secret, `<key id>:<key secret>`. A credential that isn't in that shape is refused before anything
is sent, and a provider message that echoes either part is withheld.

**Cost.** Higgsfield bills in credits whose price depends on the account's plan, so the catalog
prices these models as unknown before execution rather than baking in a plan's rate.
`Higgsfield::estimate` asks Higgsfield's estimate endpoint about the exact body a submit would
send (credits and USD, rounded up to the micro). It is the adapter's `Provider::quote`, so a run's
confirmation asks it first (see *Provider quotes* below). An image an estimate uploads is kept,
by content, for the next submit that sends it, so quoting a run doesn't upload twice.

**Genjutsu edits (#179).** Each mode declares its source video edit in the catalog (`edit`: the
operation, the source role, the shortest source, how much of a longer one is used, the smallest
frame and what happens to the sound). All three take sources of at least 4 s and use only the
first 30 s; object swap also needs frames of at least 409,600 pixels. A wired source whose length or
frame size is known (imported or generated MP4, QuickTime, Matroska and WebM files have theirs read from the headers) is
checked before anything is sent. Restyle keeps the source's sound; Higgsfield doesn't document what
motion transfer and object swap do with it, so the picker says so. The price is Higgsfield's list price per started second of the part of the
source that is used: 0.318 USD at 480p, 0.681 USD at 720p and 1.632 USD at 1080p (checked
2026-10-21). An account's credits can make it cheaper, so the quote is the most a run can cost. A
source made earlier in the same run, or whose length couldn't be read, leaves the step unpriced
until it is known.

**Restyle presets.** Restyle needs one of Higgsfield's style presets, which its API lists
(`GET /models/higgsfield/genjutsu/restyle/v1.0/presets`, free). **Refresh option lists** fetches
them as the `higgsfield.restyle_presets` list: each preset's value is its id, which the card
stores and the request sends, and its label is the preset's name. Once fetched, the card's
Preset field is a picker showing the names (#676), and `node.options` lists them too. Entries whose id isn't a UUID, or whose name is blank, over 256 bytes, has
control characters or echoes the key, are left out and counted; a list over 1,024 presets is
refused whole. A restyle without a preset, or with one that isn't a UUID, is refused before
anything is uploaded or paid, and a preset the latest list doesn't have is reported on the card.

**Soul IDs (#180).** A Soul ID is an identity trained on the Higgsfield account. SOUL 2's
optional **Soul ID** field (`custom_reference_id`) names one, and **Soul ID strength**
(`custom_reference_strength`, 0 to 1, default 1) is sent only with one. **Refresh option lists**
fetches the account's ready ones as the `higgsfield.soul_ids` list, valued by id and labelled by
name, from `GET /v1/custom-references/list` (free; pages of 50 until an empty one, at most 20). Higgsfield documents the
reference object (`id`, `name`, `model_version`, `status`) but not the list's reply, so a page must
be a list of those objects, bare or under `items`; any other shape, and an account past 1,000, is
refused rather than listed in part. Only `model_version: "v2"` entries whose `status` is
`completed` are listed; ones still training, failed or trained for another SOUL are left out, and
entries that don't read as a reference are counted. Soul IDs belong to the account that trained
them, so before a run is paid the adapter reads the documented `GET /v1/custom-references/{id}`
once, when it stages the body: an id the account doesn't have, one still training (or in a state
Higgsfield doesn't document), one whose training failed (with Higgsfield's reason) or one trained
for another SOUL is refused with that reason, and nothing is billed. A retry of the same job
replays the checked body.

**Training Soul IDs (#680).** A Train style card on **Higgsfield Soul ID training** trains one from its
wired pictures (1–100 PNG, JPEG or WebP, each staged like Kling's image) under the card's name (1–84
characters). It is quoted at Higgsfield's list price, 2.50 USD a request (checked 2026-10-22), and
sends `model_version: "v2"`. Higgsfield documents no idempotency key for creation, so the name sent
carries a tag from the run's idempotency key (`Mara [nc-…]`), and every submit first lists the
account for it: the engine's retry after a lost or unreadable reply adopts the identity already
created instead of training and paying again. A POST this process sent that the list doesn't show
yet fails the job rather than risk a second charge, and says to look for the identity before
training again, since a new run has a new key and trains (and bills) a new one. Only a 400, 401,
403 or 422 that Higgsfield explains counts as refused, letting the same run send it again. The card polls `GET /v1/custom-references/{id}`
until training completes, and keeps a style descriptor naming the identity; a failed training
shows Higgsfield's reason on the card. Training can't be cancelled. When the desktop app records a
training it refreshes the option lists, so the new identity appears in SOUL 2's Soul ID picker,
labelled without the tag, once it is ready.

**Inputs.** Kling's image (PNG, JPEG or WebP whose bytes match their type, at most 20 MiB) is
staged through Higgsfield's upload grant: the adapter asks for an upload link with the key, then
PUTs the bytes to the storage host **without** the key, with only the grant's content type and
storage headers (`x-amz-*`, `x-goog-*`). Both links must be public `https`, and the grant must
name the type asked for. The request names the public link. A Genjutsu source is staged the same way, first and as
`video/mp4` (Higgsfield's uploads take no other video type, so convert a QuickTime or WebM clip
first; planning refuses any other type before anything is sent). Higgsfield takes sources of up to
200 MiB, but NodeCraft reads at most 64 MiB of media for one job, the source and its images
together, so keep the source under that (`run.quote` shows a run over it as blocked). Its
reference images follow in order, at most 20 MiB each. Uploads may take two minutes plus 8 ms
per KiB, enough for a 1 Mbit/s uplink.

**Idempotency and replay.** Each submit carries the run's idempotency key (hashed if Higgsfield
wouldn't accept it as is). The staged body is kept per key and job, so a retry after a lost reply
sends the same bytes under the same key without uploading again, and Higgsfield returns the
original request instead of starting a second one; the engine therefore retries an ambiguous submit.
A different job under the same key is staged afresh. Before the first POST the body is also saved
in the run journal (#664), so a submit cut off by a crash or quit is sent again after the next
launch's resume: the same bytes under the same key, without uploading again, so Higgsfield bills it
once. If the request can't be saved, it isn't sent. If Higgsfield refuses the saved request (its
upload links may have expired), that is reported as possibly billed and nothing is uploaded or sent
anew. A request staged by an older NodeCraft, or one that only lived in memory, still can't be
replayed: after a restart a replay would restage the image, its link would change, and Higgsfield
would answer that the key was used with different parameters. That reply is reported as "may have
been billed" and never retried, so a second charge can't happen silently.

**Replay after a restart, for any adapter.** An adapter that sends a body it built from uploads calls
`SubmitContext::stage` with that exact body (at most 1 MiB) before `paid()`; the engine saves it
with the job in the run journal and refuses the send if that save fails. After a restart the job
has no handle but a staged request, and `Provider::replay` sends it unchanged. Adapters that don't
stage keep the old rule: a job with no handle after a restart is settled as unresolved, never sent
again.

**Errors.** Replies that prove nothing was accepted (400 including "too many concurrent requests",
which waits 15 s, 401 key, 403 credits, 404, 422 validation, 423 blocked, 503 disabled), each with
Higgsfield's reason, release the reservation; anything else, including 5xx and unreadable bodies,
keeps it as possibly billed.

**Status and results.** Polling is every five seconds while queued and three while running. The
status reply must name the request it was asked about. Status and cancel links in replies are
ignored: the key only goes to links the adapter builds from the checked request id. Completed SOUL requests return one to four images and the video models one
MP4; each is downloaded without the key from a public `https` host, checked against the expected
media type and capped (256 MiB each, 512 MiB together), and saved into the project. Higgsfield keeps
results for at least seven days, so a failed download is retried on the next poll. Failed,
moderated (`nsfw`) and cancelled requests report as such.

**Cancel** works only while a request is queued; once it has started, Higgsfield's 400 with its
reason is reported as "runs on and is billed". Any other 400 is an ordinary error.

Higgsfield is native only, like the other storage-backed adapters. Training a Soul ID is #680.

**Not verified live yet:** the request shapes come from Higgsfield's public docs; whether the
account can use each model, and the exact upload header set, are untested here.

The ignored live test requires `NODECRAFT_LIVE_HIGGSFIELD=1`,
`NODECRAFT_LIVE_HIGGSFIELD_ACKNOWLEDGE_COST=1` and `HIGGSFIELD_API_KEY` (`id:secret`), plus separate
explicit human spending authorization (one SOUL image). It never runs in CI:

```sh
NODECRAFT_LIVE_HIGGSFIELD=1 NODECRAFT_LIVE_HIGGSFIELD_ACKNOWLEDGE_COST=1 HIGGSFIELD_API_KEY=… \
  cargo test -p nodecraft-providers --features http,higgsfield --test live_higgsfield -- --ignored
```

## ElevenLabs speech, sound and music

The adapter tracker [#20](https://github.com/CypherNaught-0x/nodecraft/issues/20) is implemented
across the following merged changes. Generation works in the desktop app and headless CLI.
The desktop app and browser preview request the account voice list; streamed previews are
desktop-only. The CLI does not request either capability. The host limits below apply.

| Tracker requirement | Implementation and offline coverage |
|---|---|
| Speech, effects, music and voice changing | [#667](https://github.com/CypherNaught-0x/nodecraft/pull/667); [adapter contract tests](../crates/providers/src/elevenlabs/tests.rs) and [catalog](../assets/catalog/elevenlabs.toml) |
| Voices from the account | [#857](https://github.com/CypherNaught-0x/nodecraft/pull/857); [option-list integration tests](../crates/engine/tests/option_fetch529.rs) |
| Streaming speech previews | [#876](https://github.com/CypherNaught-0x/nodecraft/pull/876) and [#884](https://github.com/CypherNaught-0x/nodecraft/pull/884); [stream contracts](../crates/providers/src/elevenlabs/stream_tests.rs), [engine delivery tests](../crates/engine/tests/audio_previews875.rs) and [card playback tests](../crates/ui-egui/src/app/audio875_tests.rs) |

The [opt-in live tests](../crates/providers/tests/live_elevenlabs.rs) cover a short speech request
and streamed speech. Their existence is not evidence of a live run; paid validation requires
the explicit authorization described below. The tests linked above use fixture or mock data.

The optional `elevenlabs` adapter calls the [ElevenLabs API](https://elevenlabs.io/docs/api-reference)
at the fixed origin `https://api.elevenlabs.io` with the key in the `xi-api-key` header. Every
request is synchronous: the reply body is the audio, always asked for as MP3 at 44.1 kHz and
128 kbps (`output_format=mp3_44100_128`), and must sniff as MP3 or it is refused as malformed.
Six catalog models share it, one per model or endpoint:

| Catalog model | Endpoint | Input | Estimate |
|---|---|---|---|
| `elevenlabs.v3` | `POST /v1/text-to-speech/{voice}` | text, at most 5,000 characters | 0.08 USD per 1,000 characters |
| `elevenlabs.multilingual-v2` | same | text, at most 10,000 characters | 0.08 USD per 1,000 characters |
| `elevenlabs.flash-v2-5` | same | text, at most 40,000 characters | 0.04 USD per 1,000 characters |
| `elevenlabs.sound-effects` | `POST /v1/sound-generation` | a description, 0.5–30 s, optional loop | 0.002 USD per second (0.12 USD per minute) |
| `elevenlabs.music` | `POST /v1/music` | a description, 3–600 s, optional instrumental-only | 0.0025 USD per second |
| `elevenlabs.voice-changer` | `POST /v1/speech-to-speech/{voice}` (multipart) | one recording | by usage (0.12 USD per minute of input) |

Prices are from the [API pricing page](https://elevenlabs.io/pricing/api), checked 2026-10-21.
Speech is priced per 1,000 characters. Admission counts the known prompt's Unicode characters
after trimming it, just as the adapter does; a prompt made earlier in the same run stays
unpriced until it is known. A fanned step reserves the most expensive prompt for each job.
The result records the characters sent as its billable units. Sound effects are 0.12 USD per minute of sound, so the
quote follows the chosen duration. Eleven v3 takes only three stabilities, 0 (creative), 0.5
(natural) and 1 (robust); its slider steps between them and any other value is refused before
sending. Descriptions for sound effects and music are capped locally at 5,000
characters.

Speech and the voice changer take a voice: one of ten premade voices, or a voice id typed in to
override it (ASCII letters and digits, at most 64, so it can't change the request path). The
sliders map onto `voice_settings` (stability, similarity, style and speed 0.7–1.2; the voice
changer sends stability and similarity only). Eleven v3 and Flash v2.5 take an optional ISO 639-1
language code; Multilingual v2 detects the language and is never sent one. The voice changer
uploads MP3, WAV, OGG, FLAC, M4A or AAC, judged by its bytes, at most 50 MiB; it may remove
background noise first.

**The account's voices (#670).** **Your voice** (`voice_id`) names the `elevenlabs.voices` option
list, so **Refresh option lists** fetches the voices the account can use: its own clones and
designed voices, ones added from the library, and the premade ones. The list is free: it pages
through `GET /v2/voices` (100 a page, following `next_page_token`, at most 50 pages) with only the
key sent, and each page is capped at 8 MiB. Voices are valued by id and labelled by name; an
entry whose id isn't letters and digits, or that has no name, is skipped and counted, and a name
or id that echoes the key is never listed. Once fetched, the field is a picker, and a typed id
not in the list is refused before a run; before a fetch it stays a text box.

There is no idempotency key and nothing to poll or cancel, so a submit is never retried
automatically: once the paid request is sent a lost reply may have been billed. The exceptions
are ElevenLabs's documented refusals, which release the reservation: 401 with `detail.status`
`invalid_api_key` or `quota_exceeded`; a 422 validation list (`detail: [{loc, msg, type}]`); and
400 or 429 with `quota_exceeded`, `voice_not_found`, `too_many_concurrent_requests`,
`system_busy` or `max_character_limit_exceeded`. Error replies map onto the shared kinds (auth,
quota as budget, rate limit and busy with `Retry-After`, input errors, server); their messages
are cut short and withheld when they echo the key. Replies over 32 MiB are refused (ten minutes of
music is about 10 MB). Limits can only be lowered.

**Streamed speech previews (#671).** When the runner asks for previews
(`RunLimits::stream_previews`), speech is sent to `POST /v1/text-to-speech/{voice}/stream`
instead, with the same body, and the MP3 is reported as it arrives as
`JobProgress::Running { partial }`: first once 64 KiB (about four seconds) has come, then each
time the audio received doubles, at most ten times, so the previews together copy at most twice
the reply. A preview is reported only once the audio sniffs as MP3. The reply is gathered whole,
capped at the same 32 MiB and read exactly as the one sent at once, so the result is unchanged. It
goes through the same no-redirect transport; a stream cut short is a lost reply, possibly billed,
and keeps the previews already reported. Sound effects, music and the voice changer never stream.
The audio before each chunk is previewed only once that chunk arrives, so the last chunk never
makes a preview: a transport that can't stream hands the whole reply over as one chunk and makes
none, rather than one equal to the result. The desktop app's runner asks for previews and its
speech cards play them while the sound arrives (#875); the CLI's and the browser's don't.

The ignored live tests speak one short sentence with Flash v2.5 (about 0.002 USD) and stream one
through `/stream` with Eleven v3 as `mp3_44100_128`, checking that its previews grow and are never
the whole reply. They require
`NODECRAFT_LIVE_ELEVENLABS=1` and `ELEVENLABS_API_KEY`, plus explicit human spending
authorization, and never runs in CI:

```sh
NODECRAFT_LIVE_ELEVENLABS=1 ELEVENLABS_API_KEY=… \
  cargo test -p nodecraft-providers --features http,elevenlabs --test live_elevenlabs -- --ignored
```

## Editing ComfyUI workflow cards

The card menu's **Edit workflow fields…** (#947) opens media inputs, parameters and results.
Show or hide fields, edit their names and use Up/Down to reorder them, then Apply fields.
Cancel leaves the card untouched. Filter fields searches their names and workflow targets.
Read server nodes, then Reload fields, when the editor has no catalog for the card's server.
The saved mapping travels with the project and Techniques. Applying it is one Undo step;
current values survive hiding, and renamed ports keep their wires. The editor asks explicitly
before disconnecting wires or clearing history after a change of result media types. Finish or
cancel a card's run before editing its fields.

New imports expose image/video/audio upload inputs, prompts (including single-line prompt
widgets), seeds, steps, CFG and dimensions. Other scalar widgets, such as reference data,
model choices and custom settings, can be exposed in the editor. Internally linked inputs stay
inside the workflow. Compatible core `PreviewImage` nodes become `SaveImage` nodes in the
imported copy so PBR previews are retained as results; the source workflow file is untouched.
This conversion requires the server to describe both core nodes with the expected image
connection and filename widget. Custom or incompatible previews are left unchanged.

Results remain one list port per media type. Selected saver order is saved in the job handle,
so a resumed local ComfyUI job keeps the same image order. Comfy Cloud also honors that order
when its response supplies node ids; results without node ids retain the server's order after
identified results. Older saved handles retain their previous ordering. Renaming a media list
port writes workflow config version 2, which older NodeCraft builds refuse as unsupported.
For programmatic editing and explicit disconnection/history flags, see [MCP](mcp.md).

## Comfy Cloud workflows

The optional `comfy-cloud` adapter (#16, #865) runs ComfyUI workflows on Comfy's hosted GPUs through
its versioned v2 API at the fixed base `https://cloud.comfy.org`. In **API keys**, save your Comfy
API key under **comfy-cloud**. Open **ComfyUI**, choose **Add Comfy Cloud**, then **Make default**
to import workflows against its node list. Existing Workflow cards can choose the named profile
in their **ComfyUI server** menu. The same Workflow catalog entry, mapping, parameters, media
ports and selected outputs work with either backend; no separate hosted model needs to be added.

The profile is saved in settings with `backend = "cloud"`; the key stays in the OS keychain.
Its address is fixed and cannot be edited. Older profiles without `backend` remain local.
Serverless deployment URLs are not supported by this profile. Checking it reads the node list
without submitting a job; local GPU statistics and queue health are not available for this API.

At the adapter boundary, both the Workflow card's `comfy.workflow`, `comfy.values` and
`comfy.outputs` text inputs and the direct API shape (`params.workflow`, `params.values`) are
supported. Media roles name upload inputs such as `1.image`. Built-in ComfyUI presets use the
same template expansion as local servers and still require their models to exist on the host.

- **Checked first:** the workflow and its overrides are checked against the cloud's
  `/api/object_info` (cached for five minutes) before anything is uploaded, so a missing node or a
  value the cloud doesn't offer costs nothing.
- **Inputs** are sniffed (HDR inputs are refused as for local ComfyUI), conservatively capped at 50 MiB each,
  uploaded to `POST /api/v2/assets` under content-hash names, and referenced from the workflow as
  `core/ASSET` values. Uploads are free and deduplicated, so a failed upload proves no job exists.
- **Submitting once:** `POST /api/v2/jobs` carries an `Idempotency-Key` derived from the run's own
  key. Comfy takes the first request with a key and refuses any later one
  (`idempotency_key_reuse`) without running it, so the same run is never charged twice. Comfy
  Cloud can't look a job up by key, so a submit whose reply was lost is reported as possibly
  billed and never retried automatically (`retry_ambiguous_submit` is false); a re-run gets a new
  key. Comfy's documented refusals that create no job and release the key (429 `queue_full` or
  `rate_limited`, 402 `insufficient_credits`, 422 `invalid_workflow`) are proof that nothing was
  accepted: the reservation is released, and a full queue or rate limit is retried with back-off
  (#631). A 5xx, an unreadable body or a code at another status stays possibly billed.
- **Progress and results:** polling `GET /api/v2/jobs/{id}` reports queue position, the progress
  fraction and message, and failures with the node that failed. Finished outputs are downloaded
  from `/api/v2/assets/{id}/content` with the key, and its redirect to a signed `https` storage
  URL is followed without it (no request follows redirects on its own). Outputs are capped per
  file and in total (256 MiB, all NodeCraft keeps of one run) and in count (256), sniffed, and latents are skipped; an
  expired signed link is a retryable failure so the next poll fetches fresh ones, while Comfy's own
  refusal of a download (a revoked key, an expired asset, blocked content) is reported as such.
- **Cancel** posts to `/api/v2/jobs/{id}/cancel`.
- **Not sent:** the key in `extra_data`. Partner (API) nodes bill extra credits that no NodeCraft
  quote covers, so a workflow using one (`api_node` in the cloud's node list) is refused before
  anything is uploaded or run.

Comfy Cloud bills GPU time, which isn't known before a run. The physical provider binding marks
this work **unpriced**, even when the reused catalog entry is free for a local server and no
node-list check has completed. A finite budget refuses the run before any request. With no
finite budget, the normal cost confirmation requires explicit acceptance of the unknown-priced
jobs; it never presents hosted work as free. Changing profiles invalidates earlier quotes and
prepared runs. Comfy's bill is not yet imported as a settled GPU usage charge in NodeCraft.

The [official v2 API schema](https://github.com/Comfy-Org/docs/blob/main/openapi-v2.yaml) and
[SDK contract](https://github.com/Comfy-Org/docs/blob/main/development/api-development/sdks.mdx)
were checked on 2026-10-24. Upload tags are a JSON array; returned files are ordered by their
workflow node id to preserve the Workflow card's output mapping.

The live smoke test only reads the node list, so it spends no credits:

```sh
NODECRAFT_LIVE_COMFYUI_CLOUD=1 COMFY_CLOUD_API_KEY=… \
  cargo test -p nodecraft-providers --features http,comfy-cloud --test live_comfy_cloud -- --ignored
```

The older `NODECRAFT_LIVE_COMFY_CLOUD` opt-in remains accepted. Neither test runs in CI.

## Luma through fal (#26)

Use the existing `fal` API key account. The Luma catalog entries reuse fal's shipping adapter,
queue, cancellation and resumable handles; they do not need a separate Luma key. The aliases
are live rather than pinned model versions:

| Model | Admitted input | Standard output and quote |
|---|---|---|
| Luma Ray 3.2 | One prompt | 5 or 10-second MP4; 540p, 720p or 1080p at 0.10, 0.20 or 0.40 USD/second |
| Luma Ray 3.2 frames | Prompt, first frame, optional last frame | 5-second MP4 at the same resolution rates |
| Luma Photon | One prompt | One image; 0.019 USD/megapixel, but dimensions are service-selected, so the upfront quote is unknown |

Prices and request contracts were checked 2026-10-24 against the official
[Ray text](https://fal.ai/models/luma/agent/ray/v3.2/text-to-video),
[Ray frames API](https://fal.ai/models/luma/agent/ray/v3.2/image-to-video/api), and
[Photon](https://fal.ai/models/fal-ai/luma-photon) pages. Ray duration is sent explicitly.
Loops require five seconds and no last frame. Ten-second frame animation requires the service's
multi-keyframe API, which is not offered here. HDR, EXR, references and editing are also outside
these modes. Safety screening keeps the service default.

Ray prompts are 1–6,000 characters and Photon prompts 3–5,000, after surrounding whitespace is
trimmed. The shared aggregate text-metadata limit is still 16 KiB. Frames must be PNG/JPEG/WebP,
at most 1 MiB and 1024 pixels per side; they are decoded before payment. Results keep fal's
16 MiB per-file cap for Photon. Ray text/frame clips instead have a 256 MiB per-file and
aggregate cap, also when resumed from a saved handle. Application limits may lower either cap.
Photon images are decoded under the 4096-pixel/16,777,216-total-pixel bounds,
and may be PNG, JPEG or WebP. The generic Photon file schema need not report dimensions; actual
bytes determine them. Ray uses the existing bounded MP4 container check, not a full video decode.

Submissions use the full model path, while Ray's authenticated queue routes use `luma/agent`
and Photon uses `fal-ai/luma-photon`, following the
[queue contract](https://fal.ai/docs/inference-api/queue/submit-path).
Downloads use the existing credential-free HTTPS `fal.media` allowlist and refuse redirects.
An ambiguous paid submission is never replayed. Cancellation is best effort and does not prove
a refund. Returned billable units remain evidence, not an invented monetary invoice. The builtin
browser transport cannot provide no-redirect requests, so these modes require an audited native
transport even though the library builds for wasm.

No paid live calls were made for this change. Ignored `live_luma` tests require both
`NODECRAFT_LIVE_LUMA=1` and `NODECRAFT_LIVE_LUMA_ACKNOWLEDGE_COST=1`, an explicit `FAL_KEY`, and
spending authorization. The Ray smoke requests a 0.50 USD clip; Photon has no exact upfront
total. They never run in CI.

## Deepgram file transcription (`deepgram` feature)

Only a bounded typed error envelope with a recognized matching status/code proves a refusal
before transcription: auth (401/403), credits (402), invalid input (400/422) or rate limit (429).
These release the reservation; a recognized rate limit can retry through the existing scheduler.
Status alone, unknown codes, malformed/oversized bodies, transport loss and server errors keep
possibly-billed evidence and never authorize an ambiguous retry. Provider error text is omitted.
See the [documented error contracts](https://developers.deepgram.com/docs/errors).


The Deepgram Nova-3 transcription card sends one audio file directly to
`https://api.deepgram.com/v1/listen`. Save an API key under the `deepgram` account in
API keys. The fixed-origin request uses secret `Authorization: Token` credentials and
never follows redirects. This provider has native host setup; the browser has no builtin
transport/key setup for it.

The current card transcribes English with smart formatting. It accepts one WAV, MP3,
FLAC or Ogg input, up to 25 MiB, with readable headers reporting 1–2 channels,
8–192 kHz and a positive duration of at most 30 minutes. These are local limits,
not Deepgram service limits. Header inspection is bounded; Deepgram validates the
encoded audio. Stereo is mixed into one transcript. No prompt, URL input, callbacks,
diarization, live streaming or optional analysis features are sent. Empty text is a
valid result for silence. Responses are capped at 4 MiB and transcripts at 1 MiB.

Every request uses `mip_opt_out=true` to exclude the recording from Deepgram's Model
Improvement Program. Account rates and opt-out can affect pricing. The catalog is
explicitly usage priced: a run needs unknown-price acceptance and neither duration
metadata nor a missing monetary receipt is converted into an invented dollar charge.
The adapter makes one synchronous request with a ten-minute transport timeout. A lost
reply may have been billed, is not automatically retried and has no resumable handle.
Local cancellation cannot confirm remote cancellation or a refund.

Offline fixtures exercise request bounds, transcript parsing, error mapping, actual
host recording, unknown-price admission, no replay after a lost reply and picker selection.
The ignored live smoke requires explicit spending authorization, `NODECRAFT_LIVE_DEEPGRAM=1`
and `DEEPGRAM_API_KEY`; it sends the repository's public tone fixture, never a private file.
No paid live call was made while implementing this adapter.

Sources checked 2026-10-24: [file transcription](https://developers.deepgram.com/docs/pre-recorded-audio),
[request schema](https://developers.deepgram.com/reference/speech-to-text/listen-pre-recorded),
[audio formats](https://developers.deepgram.com/docs/supported-audio-formats),
[model-improvement opt-out](https://developers.deepgram.com/docs/the-deepgram-model-improvement-partnership-program)
and [pricing](https://deepgram.com/pricing).
## OpenAI speech and file transcription (`openai-audio`)

Add an OpenAI key under **API keys → openai**. The native host binds it to the
`openai-audio` catalog provider. Choose **OpenAI speech (ends Jan 6, 2027)** on a Generate audio
card with a prompt, or connect a recording to a Generate text card and choose **OpenAI file
transcription**. These use the same quote, acceptance, budget and result-history commands as
other generated media. The adapter compiles for wasm, but a custom browser host must supply
transport/routing; the shipping browser does not configure paid credentials here.

Speech uses `gpt-4o-mini-tts-2025-12-15` at `/v1/audio/speech`, a built-in voice, speed from
0.25 to 4 and an optional style instruction. Input is limited to 4,096 characters and output
is MP3. The catalog identifies it as AI-generated speech; disclose that to listeners.
**OpenAI has deprecated these speech models and will shut them down on January 6, 2027.**
Migration is needed before that date. The suggested Realtime replacement is a separate API,
not a model accepted by this endpoint, and is not implemented here. See the
[speech guide](https://developers.openai.com/api/docs/guides/text-to-speech) and
[deprecation notice](https://developers.openai.com/api/docs/deprecations), checked 2026-10-24.

Transcription uses current `gpt-transcribe` at `/v1/audio/transcriptions`, uploading one MP3,
WAV, FLAC, Ogg or M4A recording up to 25 MB, plus an optional context prompt. It returns plain
text in the card history. This initial integration does not expose WebM/video containers,
translation, diarization or realtime transcription. See the
[file transcription guide](https://developers.openai.com/api/docs/guides/speech-to-text).

Both entries show **Usage based (USD)**, requiring acceptance of one unpriced job; there is
no guaranteed price ceiling. Speech does not return billable token counts here. Transcription
records [the published $0.0045/minute rate](https://developers.openai.com/api/docs/models/gpt-transcribe)
only if its response includes valid duration usage; otherwise the cost remains unknown.
Requests never automatically replay after an uncertain paid submission, do not follow
redirects, and cannot cancel a request already sent. Parsed authentication, validation, quota
and rate-limit refusals release the reservation; documented transient rate-limit refusals can
retry under the configured attempt limit. Unknown or oversized error bodies, server errors and
lost replies remain possibly billed. For those uncertain failures, retry manually only after
checking the provider account for any charge. See OpenAI's
[error codes](https://developers.openai.com/api/docs/guides/error-codes).

Offline fixtures cover request shapes, validation and size bounds, redacted errors, duration
billing and actual canvas runs in both directions. The ignored `live_openai_audio` smoke makes
two paid requests only with separately authorized spending, `OPENAI_API_KEY`, and both
`NODECRAFT_LIVE_OPENAI_AUDIO=1` and `NODECRAFT_LIVE_OPENAI_AUDIO_ACKNOWLEDGE_COST=1`.
No live service availability or quality claim follows from offline tests.

## Black Forest Labs: direct FLUX.2 klein 4B (#33)

In **API keys**, save your Black Forest Labs key under account `bfl`, then select
**FLUX.2 klein 4B (BFL)** on an image generation card. This uses BFL directly, with no
aggregator account. The built-in catalog supports one text prompt (up to 16 KiB), optional
32-bit seed, and width/height from 64 to 1984 pixels in steps of 16. It requests one PNG.
Image references, editing, webhooks and custom endpoints are not exposed by this adapter.

The [official generation API](https://docs.bfl.ai/quick_start/generating_images) returns a
job and its cluster polling URL. NodeCraft validates and saves those, polls the same task
after restart, then downloads the signed result immediately. Only BFL API cluster hosts
receive the key; image downloads use the documented `delivery.*.bfl.ai` service without
credentials or redirects. Replies are capped at 64 KiB, image bytes at 32 MiB, decoded
image allocation at 64 MiB, and output geometry must match the request. Native desktop
transport is supported; the built-in browser transport returns an unsupported error.

[BFL pricing](https://bfl.ai/pricing?category=flux.2) depends on pixels and credits.
NodeCraft currently marks the cost as unknown usage pricing. Running requires explicit
acceptance of an unpriced job; a finite budget cannot admit it. The API's credit fields
are not recorded as dollars. Once the paid request is sent, lost replies are never retried
automatically: BFL documents no idempotency key. Cancellation stops local waiting but
cannot confirm a remote stop or prevent billing. A successful saved handle can be resumed
without sending another create request.

Offline fixtures exercise request bounds, queued/running/failure states, saved handles,
credential isolation, corrupt/oversized output, ambiguous paid replies and the real RunHost.
An ignored paid smoke test requires both explicit spending authorization and
`NODECRAFT_LIVE_BFL=1` with `BFL_API_KEY` supplied privately. It is never run in CI.
The current endpoint and statuses were verified against the
[official OpenAPI](https://api.bfl.ai/openapi.json) on 2026-10-24.

## Runway video (#25)

Store a Runway developer API key under `runway` in API keys. The desktop, CLI and headless
provider setup route the builtin `runway.gen4-5` and `runway.gen4-turbo` models to the same
adapter. Gen-4.5 accepts a prompt alone or a prompt with one first-frame image; Turbo requires
that image. Prompts are limited to 1,000 UTF-16 units. Inputs are PNG/JPEG/WebP, at most 3 MiB
and 4096 pixels per side. These are NodeCraft's local bounds; larger inputs must be resized.
The catalog offers Gen-4.5's shared landscape/portrait sizes and Turbo's six frame sizes.
Video editing, reference sets, audio generation and premium output formats are not offered.

Duration is sent explicitly (2–10 seconds) and the catalog quotes standard MP4 at 0.12 USD per
second for Gen-4.5 and 0.05 USD per second for Turbo, excluding tax. Sources checked 2026-10-24:
[Runway pricing](https://docs.dev.runwayml.com/guides/pricing/) and
[the API schema](https://docs.dev.runwayml.com/openapi.json). The API's optional terminal
nonnegative credit count is recorded as cost at 0.01 USD/credit when representable. Missing cost,
negative credit adjustments or an overflowing conversion leave usage unknown and retain the
conservative estimate; they do not discard a successful video. Turbo omits the unsupported
`outputFormat` field and uses its standard MP4 default.
Input safety refusals may be billed. No paid live verification was performed for this change.

One paid create returns a resumable UUID. The shared runner polls at five-second intervals.
A lost reply may hide a billed task: Runway has no documented submission deduplication, so an
uncertain create is not retried. A failed result download resumes the task and obtains a fresh
link. JSON replies are limited to 64 KiB; videos to 256 MiB. The downloaded MP4 must have readable
headers, dimensions no greater than 4096 pixels and duration no greater than 60 seconds; this
checks the container metadata, not every encoded frame. Only public CloudFront HTTPS result
links are admitted. API credentials never accompany downloads and redirects are refused.
The builtin browser transport cannot provide no-redirect requests, so it refuses this adapter;
wasm compilation is supported for hosts with an audited transport.

Cancellation reads the task first and refuses to delete an already completed video. Runway's
DELETE can still race completion and remove that result; cancellation does not guarantee a
refund. Already missing/cancelled tasks count as stopped. See
[Runway task management](https://docs.dev.runwayml.com/api/).

`live_runway` is ignored and requires `NODECRAFT_LIVE_RUNWAY=1`,
`NODECRAFT_LIVE_RUNWAY_ACKNOWLEDGE_COST=1`, and `RUNWAYML_API_SECRET`; run it only with spending
authorization. It requests one 2-second Gen-4.5 clip (0.24 USD before tax), never in CI.

## Hugging Face text (`huggingface` feature)

The builtin GPT OSS 120B (Groq) card calls
`https://router.huggingface.co/v1/chat/completions` using the existing OpenAI-compatible
chat client. Its explicit model id `openai/gpt-oss-120b:groq` selects Groq through the HF
router; it does not select whichever provider is fastest or cheapest. The public router
model listing reported this route live on 2026-10-24. Availability can still change.

In the native app, save an HF token with Inference Providers permission in API keys under
`huggingface`. It is sent only as a secret Authorization bearer header. Project files never
contain the token. The browser has no builtin transport/key setup for this catalog provider.

The card accepts text only: at most 64 turns and 1 MiB total text. Requested completion tokens
are limited to 1–8,192, default 2,048; reasoning can consume part of the allowance. The existing
client bounds responses and streaming, retains token usage, and maps payment-required to Budget,
auth failures to Auth and ordinary 429s to RateLimited (quota-coded 429s to Budget).
Pricing is explicitly ByUsage: account credits, changing rates and provider-specific usage mean
this adapter does not claim a dollar quote or actual charge. Normal unknown-price acceptance is
required. No ambiguous paid submit is replayed, no polling handle exists and local cancellation
cannot confirm a remote stop or refund. Images, tools, embeddings and other HF tasks are outside
this text adapter.

Offline fixtures cover routing, request bounds, streaming, errors, real host admission and the
lost-reply driver path. The opt-in smoke test requires `NODECRAFT_LIVE_HUGGINGFACE=1` and
`HF_TOKEN`; ordinary tests do not make paid requests.

Contract references: [router and provider selection](https://huggingface.co/docs/inference-providers/en/index),
[chat request schema](https://huggingface.co/docs/inference-providers/tasks/chat-completion),
[billing](https://huggingface.co/docs/inference-providers/pricing).

## Gemini text (`gemini-llm` feature)

The Gemini 3.8 Flash text card (#44) calls Google directly through its documented
OpenAI-compatible chat endpoint. Add the API key to the existing `gemini` account in
API keys, choose **Gemini 3.8 Flash** on a text generation card, and accept the usage-based
price before running. No gateway or new SDK is needed. The native host is wired; browser
provider settings do not expose this adapter yet.

Only text is advertised: at most 64 input turns and 1 MiB total text, with a requested
`max_tokens` between 1 and 65,536 (default 4,096). The host's usual streaming text path is
supported. Images, PDFs, tools, thought-signature replay, grounding and Interactions state
are outside this card's scope. Output uses the shared chat response/stream byte caps.

Google returns token usage; this adapter does not infer an exact dollar cost across account
tiers and thinking usage. Quotes therefore remain explicitly unknown, never free. Lost,
malformed or interrupted replies are never automatically resubmitted. A sent synchronous
request cannot confirm remote cancellation and has no resumable polling handle. Credentials
use secret headers and the shared transport's redirect protections.

Offline contract, real host-routing, unknown-price admission, lost-reply driver and actual
picker tests run in CI. The opt-in smoke test makes one real request and may cost money:
`NODECRAFT_LIVE_GEMINI_LLM=1 GEMINI_API_KEY=… cargo test -p nodecraft-providers --features
http,gemini-llm --test live_llm live_gemini44`. It is disabled unless explicitly enabled.

Contract checked 2026-10-24: [compatibility API](https://ai.google.dev/gemini-api/docs/openai),
[model limits](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash),
[pricing](https://ai.google.dev/gemini-api/docs/pricing). This implements #44 by adapter reuse,
not a new native `generateContent` or Interactions dialect; #489 remains a separate discussion.


## Grok Imagine through fal

Issue #31 uses the existing `fal` provider and its stored fal key. Enable the existing fal
runtime; pick **Grok Imagine Image 2.0**, **Image 2.0 edit**, **Video 1.5**, or **Video 1.5
frame**. A direct xAI key is not a fal credential. The catalog and adapter build for WebAssembly,
but the builtin browser HTTP transport refuses no-redirect requests before fetch. These models
need the native fal runtime or an audited host transport that provides that guarantee.

Contracts and prices checked **2026-10-27** against the official
[image schema](https://fal.ai/models/xai/grok-imagine-image/v2.0/text-to-image/api),
[edit schema](https://fal.ai/models/xai/grok-imagine-image/v2.0/edit/api),
[video schema](https://fal.ai/models/xai/grok-imagine-video/v1.5/text-to-video/api),
[first-frame schema](https://fal.ai/models/xai/grok-imagine-video/v1.5/image-to-video/api),
[image rates](https://fal.ai/grok-imagine-image-2.0), and
[video rates](https://fal.ai/grok-imagine-video-1.5).

- Image 2.0 makes 1–10 outputs, with JPEG, PNG or WebP and the schema's aspect ratios. Choose
  low/medium at 1K/2K: **$0.04/$0.06** per low-quality output and **$0.06/$0.08** per medium
  output. Image editing takes 1–5 `images` and adds **$0.01 per reference image per request**.
  The API schema supersedes older overview counts. The four presets avoid automatic quality
  and 1.5K, whose fal price is not listed in the checked table.
- Video 1.5 makes one 1–15 second MP4 with included native audio: **$0.08/$0.14/$0.25 per
  second** at 480p/720p/1080p. Frame mode requires one `first_frame`, adds **$0.01**, and follows
  its framing; it sends no aspect-ratio setting. Text mode offers seven explicit ratios.
- References are verified PNG/JPEG/WebP, at most 1 MiB and 1024 pixels per axis each. Five
  base64 edit references fit the model's 8 MiB request cap. Image results have a 16 MiB per-file,
  160 MiB aggregate and 4096-pixel per-axis cap; ten results share 167,772,160 pixels. Video
  results use the 256 MiB per-file/aggregate video cap, also when resuming saved tasks. Hosts
  may lower all these bounds.
- Quotes include reference fees from the wired values. A fixed first-frame count is known
  even before the frame finishes. An unknown upstream reference list stays unpriced and needs
  the existing explicit unpriced-job consent; finite budget caps refuse it. Fanout quotes are
  conservative per-item upper bounds.

Safety stays enabled. Native passthrough, synchronous data URLs, video reference/edit/extend
modes and Lite/legacy aliases are outside this integration. Unknown fields and roles refuse
before sending. Saved queue handles poll/cancel only canonical fal routes; output fetches use
fal storage without the API key, and redirects remain refused.

fal's [xAI model warning](https://fal.ai/models/xai/grok-imagine-image) says policy violations
can still be charged. Generation/moderation failures therefore retain possibly billed
reservations. A lost submit reply never triggers a new paid create; accepted handles resume.
Cancellation does not promise a refund. Fixtures and RunHost tests are offline; the opt-in
`grok31_live_smoke` needs `NODECRAFT_LIVE_GROK=1` and `FAL_KEY` and spends $0.04 for one image.

## LTX video through fal

Issue #30 uses the existing **fal** adapter and keychain account. Select **LTX 2.3 Pro (fal)**
for text-to-video or **LTX 2.3 Pro frames (fal)** for a first frame and an optional last frame.
Both require a prompt and offer optional generated sound. These are live Pro aliases,
`fal-ai/ltx-2.3/text-to-video` and `fal-ai/ltx-2.3/image-to-video`, checked on 2026-10-27 against
[fal's text schema](https://fal.ai/models/fal-ai/ltx-2.3/text-to-video/api) and
[frame schema](https://fal.ai/models/fal-ai/ltx-2.3/image-to-video/api).

The supported subset is 6/8/10 seconds, 1080p/1440p/2160p, 16:9 or 9:16 and 24/25/48/50 fps.
The FPS choice is stored as text in the catalog and sent as a JSON integer. Frame inputs are
PNG/JPEG/WebP, at most 1 MiB and 1024 pixels on either axis, fully decoded before submission.
Unknown settings, extra roles, missing first frames, malformed images and requests exceeding
configured geometry limits fail before HTTP. There is no seed, automatic aspect ratio, audio
input, Fast variant or native LTX credential in this integration.

[fal's model page](https://fal.ai/models/fal-ai/ltx-2.3/text-to-video) currently conflicts with
itself: its request price says 0.08 USD/s at 1080p, while its explanatory table says 0.06 USD/s.
NodeCraft therefore shows usage pricing with an **unknown total** and requires explicit
acceptance of one unpriced job. It does not invent an invoice from these rates. Billable-unit
headers remain optional observed evidence, separate from money. The configured time hint is
editorial metadata and is not a measured latency.

Accepted tasks retain fal's canonical `fal-ai/ltx-2.3/requests/<id>` status/result/cancel routes
for saved-handle resume. An ambiguous submission is not replayed; cancellation is best effort.
One MP4 result may be downloaded up to 256 MiB, with the same aggregate limit. Downloads use
fal storage only, carry no API key, reject redirects and validate the actual MP4 container.
The queue, transport and platform support remain those of the existing fal adapter.

Offline contracts cover schemas, all advertised choices, input/refusal bounds, saved-handle
polling/cancellation, output authority and error mapping. Actual RunHost fixtures cover
unknown-price consent, imported first/last frames, verified output bytes and a lost paid reply
with retries enabled. The ignored `ltx30_live_smoke` requires `NODECRAFT_LIVE_LTX=1` and `FAL_KEY`
and may charge for one six-second 1080p clip; it is never run by CI.


## Stability AI direct

The optional `stability` feature adds three first-party models on the existing cards:
**Stable Image Core** (`stability.image-core`), **Stable Audio 3 Large** (`stability.audio3`)
and **Stable Fast 3D** (`stability.fast-3d`). Add a Stability API key under the `stability`
account in API keys. **Test** uses the free authenticated balance endpoint, never a generation.
The adapter is available in the desktop app and CLI; the browser host refuses setup because its
built-in transport cannot guarantee authenticated no-redirect requests. No live API or browser
CORS behavior was tested for this change.

These are distinct from the SD3.5 Large and Audio 3 Small models offered through
[fal](https://fal.ai/models/fal-ai/stable-diffusion-v35-large) and
[fal Audio 3 Small](https://fal.ai/models/fal-ai/stable-audio-3/small/music/text-to-audio).
The aggregator inventory did not establish current official 3D coverage for the chosen model.
Using the direct APIs gives all three chosen families one key and one origin; it does not add
all Stability editors, older models or every available variant.

Rates were checked on 2026-10-27 against the
[current pricing reference](https://docs.stability.ai/pricing),
[Core operation](https://docs.stability.ai/api-reference/generate/stable-image-core),
[Audio 3 operation](https://docs.stability.ai/api-reference/stable-audio/text-to-audio),
[Fast 3D operation](https://docs.stability.ai/api-reference/3d/stable-fast-3d) and
[current OpenAPI](https://docs.stability.ai/openapi.json): one credit is 0.01 USD, with
Core 3 credits, Audio 3 8 credits and Fast 3D 10 credits per successful generation. The
[legacy pricing page](https://platform.stability.ai/pricing) still lists Audio 3 at 26 credits;
the catalog follows the migrated reference, whose pricing, operation and OpenAPI agree on 8.
These dated estimates are separate from an observed invoice and should be checked again when
updating the adapter. No claim is made that the two pricing pages agree.

Core sends one prompt (up to 10,000 characters), optional negative prompt, one of nine aspect
ratios and PNG/JPEG/WebP output, with a seed from 0 to 4,294,967,294 (0 means random).
The API returns a 1.5 MP image. The host admits up to 16 MiB and 4096 pixels per side, probes
and decodes its actual format, and refuses a filtered finish reason. Fast 3D takes one wired
JPEG/PNG/WebP image. Its host limits are 4 MiB and 64–2048 pixels per side, stricter than the
API's total-pixel limit. Texture resolution, foreground ratio, remeshing and an optional positive
vertex limit are exposed. It validates and keeps the original self-contained GLB up to 64 MiB;
embedded texture colors are sampled by the existing viewer, and export retains original bytes.

Audio 3 sends an explicit 1–380-second duration, 4–8 steps, prompt guidance and optional seed,
requesting stereo 44.1 kHz WAV. Its 128 MiB result allowance covers long WAVs without the image
adapter's smaller quota. A versioned handle persists the accepted generation id and duration.
Polling resumes that id on the same authenticated API; pending responses must identify the same
job, and WAV chunks, PCM/IEEE-float sample formats (including standard extensible headers),
frame alignment, byte rate and actual data duration are checked before history is written. Results are
available from Stability for 24 hours, according to its reference.

No redirect or remote-supplied URL is followed. All three modes use normal cost consent and
budgets. Parsed documented validation, auth, credit, moderation and rate-limit refusals release
the reservation; unparsed replies, lost creates and server errors remain possibly billed and
are not automatically submitted again. Transient audio poll failures retry the original id.
Stability documents no cancellation endpoint or create idempotency guarantee, so cancellation
cannot promise that a generation stops or is refunded. Keys and provider error bodies never
enter the project or user-facing diagnostics.

Offline contract and RunHost tests cover all three media families, cost consent, exact dated
prices, refusal billing, lost replies and audio resume. The ignored `live_stability_one_chosen_family`
smoke requires explicit spending authorization, `NODECRAFT_LIVE_STABILITY=1` and
`STABILITY_API_KEY`. `NODECRAFT_LIVE_STABILITY_MODEL` selects `stable-image-core` (default,
0.03 USD), `stable-audio-3` (0.08 USD, one second) or `stable-fast-3d` (0.10 USD, a public-safe
fixture image). The adapter itself never reads these environment variables. Live tests are not
run by CI. The API key check is documented in the official
[v1 user reference](https://prod-nlb.stability.ai/docs).
