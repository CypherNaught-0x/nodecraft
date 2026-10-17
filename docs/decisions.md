# Decisions

Decisions others will build on, newest first. The product-level ones are in
[`plan.md`](plan.md) §6; this log records the ones taken while building. Add an entry when you make
a choice a later contributor might otherwise undo.


## 2026-10-17 · MCP bridge (#116)

- **The bridge is the control channel, not a second server.** `nodecraft-cli mcp --bridge` keeps
  the MCP protocol in the CLI and forwards `command_list`, `command_run` and `canvas_inspect` to
  the app's control channel (`control::Client`). The app's session, undo and command policy
  apply unchanged, and there is one authenticated, bounded way into a running app. `ids_new`
  stays local; `screenshot` exists only when bridged.
- **Reconnect, never resend.** The app closes connections idle for 10 minutes, so the client
  reconnects before a call after 9 minutes, re-reading the token file. After a lost or
  out-of-step reply, the next call reconnects. The call that failed is reported, never resent,
  so a command can't run twice.

## 2026-10-17 · Control channel (#115)

- **Local TCP with a token file.** `nodecraft --control` binds 127.0.0.1 only, on a random port.
  It writes `control.json` (port, a 32-byte random token, pid) into the settings folder, readable
  by the owner only. Every connection starts with `hello` and the token, compared in constant
  time, or it is closed.
- **The UI thread owns the session.** Connection threads only parse and authenticate. Requests
  reach the UI thread through a bounded queue and are answered against the session the window
  shows, so changes appear live and go through the same undo path. The app crate holds the glue
  (`apps/nodecraft/src/control.rs`), since `ui-egui` and `automation` share a layer.
- **One automation policy.** The control channel reuses MCP's command listing, running and
  inspection (`AGENT_COMMANDS`/`HUMAN_ONLY`, bounded pages), so it can't do more than an agent
  can.
- **Screenshots** come from egui's viewport screenshot, encoded as PNG by `media::encode_png`. They
  are encoded off the UI thread, and they fail when the window doesn't draw.
- **Hostile local clients.** The handshake is short, everything but `hello` is refused before
  authentication, and read and write timeouts apply. Requests the client gave up on are never
  run late. The token file is written through a new file and a rename, never through a link, and
  is refused in a shared folder.
- **Stdio is deferred.** A GUI app has no useful stdin, and headless stdio automation already
  exists as `nodecraft-cli mcp`. A stdio control mode would serve the same protocol over the CLI
  later.
## 2026-10-17 · Budgets at job time (#207)

- **Reserve before submit.** `engine::jobs::ProviderRunner::with_budget` checks each priced job
  against the caps with `cost::check`. The current spend counts every reservation still open, so
  jobs running in parallel can't together overshoot a cap. The job is then recorded in the usage
  log as an `estimate`. A refusal is a non-transient failure starting with `budget: `
  (`is_budget_refusal`), so the scheduler never retries it, and it never reaches the provider.
- **Adopt what already ran.** A job resumed from a stored handle is recorded without a cap check:
  the spend already happened, and refusing would hide it.
- **Settle by what may have been billed.**
  - A finished job settles to the provider's reported cost in the estimate's currency, otherwise
    to the estimate.
  - A job the provider certainly never billed (refused at submit: a bad key, invalid input,
    cancelled before start) is released.
  - A job that got a handle, or whose submit was lost to a timeout, network or server error,
    keeps its reservation while a retry can come. When it is forgotten, settled, or requeued
    under a fresh key, it is charged its estimate as `unresolved`.
- **The host closes what the scheduler gave up on.** The driver doesn't report to the runner,
  so hosts call `ProviderRunner::settle` on terminal failures and cancellations, and `forget` when
  dropping finished jobs (see the method docs). A settle or forget that arrives while the job is
  running waits until its run ends. Estimates left open without a stored handle are settled as
  unresolved when a project is reopened.
- **A generation the provider reports as failed is assumed unbilled.** It keeps one reservation
  under its key, and a retry settles it once. Most providers don't bill failures; a provider that
  does would be undercounted by one estimate per failed attempt.
- **Free and local work isn't charged**: jobs without a `JobPrice` skip the budget. A job with an
  unknown price runs uncharged without caps and is refused under any cap.

## 2026-10-17 · MCP server (#116)

- **Generic tools, not one per command.** `command_list` and `command_run` follow the engine
  registry, so a new command is available to agents with no MCP change, and runs through the
  same `Session::dispatch` (validation, undo, events) as the UI. Techniques will get their own
  tools, since they are the user's published units.
- **Sans-IO server.** `automation::mcp::McpServer::handle` maps one JSON-RPC message to its reply;
  `serve` adds the stdio framing. The bridge and HTTP transports reuse the same handler.
- **Every command is classified for automation**: `AGENT_COMMANDS` may be listed and run, and
  `HUMAN_ONLY` never is. `HUMAN_ONLY` holds commands that take a secret (`keys.set`) or destroy
  something with no undo (`keys.remove`). An unclassified command is hidden, and a test fails
  until it is classified, so adding a command forces the decision.
- **Replies are size-capped, not just counted**: `canvas_inspect` pages stop at about 512 KiB,
  and large nodes are summarised and fetched one at a time.
- **Ids come from the host.** Commands take caller-chosen ids, so `ids_new` hands out UUIDs from a
  host-supplied random source; the CLI seeds it from the standard library's per-process random
  keys.

## 2026-10-17 · Chat turns, tools and usage (#195)

- **Finished jobs report usage.** `Submission::Finished` and `JobProgress::Done` carry a
  `Completed { outputs, usage }`, where `Usage` holds token counts and a cost in `types::Money`
  when the provider reports one. That cost is the job's actual cost for budgets; without it,
  budgets charge the estimate. A reported USD float is rounded up to the micro-dollar, so a
  charge is never free; NaN, negative and absurd amounts are ignored.
- **One conversation model for both dialects.** Tool calls, tool results and dialect-specific
  blocks (`Part::Opaque`) are parts of messages. Replies come back as an assistant `Message` to
  append unchanged: Claude's thinking blocks are bound to the conversation and must be resent
  as they came. Each dialect gets only its own opaque blocks.
- **Tools are `auto` only**, with optional `strict`. Forced tool choice is not offered: current
  Claude models return 400 for it.
- **Conversations are checked before sending**: calls are answered in the next message, results
  answer the previous one, tool names and schemas are valid, and requests are capped at 128 tools
  and 1,024 reply blocks. A malformed history fails with a clear error before anything is billed.

## 2026-10-17 · Provider keys in the keychain (#80)

- **Keys live only in the OS keychain**, service `io.github.cyphernaught0x.nodecraft`, one
  account per provider (`openai`, `comfyui:<server>`), through `keyring` pinned exactly (it wraps
  the platform APIs; we add no `unsafe`). The browser build has no keychain yet: it uses an
  in-memory store until per-origin storage with a notice (M10).
- **`Secret` never prints its value** (`Debug` shows `Secret(…)`, `Display` at most `…abcd` for
  long keys), accepts only visible ASCII up to 1 KiB (it travels in HTTP headers; Windows caps
  keychain entries), and keychain errors never echo stored bytes. The value types live in
  `providers::secrets` so adapters take a `Secret`; the OS keychain stays in the engine.
- **Keys are tested for free** (listing models), never with a paid generation.
- **`keys.set` / `keys.remove` / `keys.status`** are commands with a `keys.store` host service:
  no document change, no undo step, results only ever show a masked tail. **`keys.set` is never
  offered to the in-canvas agent or MCP clients**: a key in tool-call parameters would end up in
  LLM transcripts and logs. Keychain calls can block on a prompt, so the app registers the
  service through `secrets::KeyWorker` (#205): the request is checked at once, run on a
  keychain thread, and answered through `KeyResults::drain` under the `pending` id it
  returned. In the app, `keys.*` commands therefore answer `{pending: id}`. The host holds the
  single `KeyResults` and routes each outcome by id to whoever asked (the keys panel, the in-app
  agent), and both queues are bounded. Each nonblocking drain returns at most 64 outcomes
  (`KeyResults::MAX_BATCH`, the queue capacity), retaining later results in order for subsequent
  calls even while the worker refills the queue. Headless hosts keep the synchronous service. Settings list the accounts that have
  keys (`[keys] accounts`, names only), so the keys panel never reads every key at startup.
## 2026-10-17 · ComfyUI provider (#9, part 2)

- **Prompt ids are salted per install** (`prompt_id_for(salt, key, attempt)`): the same job always
  maps to the same id on one install, and never to another install's on a shared server. The
  salt is random and stored once in settings.
- **A submit checks the server first** (queue, then history): a queued, running or successful
  prompt is reused, so a retried submit never queues twice; a failed one moves to a new attempt
  id. Servers that ignore client-chosen ids lose this check.
- **The queue is read before the history:** a prompt leaves the queue only once it is in history,
  so a run finishing between the reads isn't reported as lost.
- **Workflow errors aren't retried automatically** (`ErrorKind::JobFailed`): running a failing
  workflow again would usually fail the same way and may cost; out-of-memory stays retryable.
- **Checks before uploads:** value overrides and `check()` against `/object_info` run before any
  input is uploaded; uploads are named by content hash, so overwriting is harmless.
- **Only `output` files are results;** a preview-only workflow fails with a clear message. Unknown
  file types are skipped; a text page served for a binary file is refused (proxy login pages).

## 2026-10-17 · LLM endpoints (#10)

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

## 2026-10-17 · Driver admission (#184)

- **Handle request construction enqueues immediately.** The returned future observes only the
  scheduler verdict and retains no sender. Abandoning an acknowledgement never cancels accepted
  work; last-handle shutdown does not wait for acknowledgement owners.
- **Shared data/control capacity feeds one FIFO.** Actual payload capacities and fixed envelope
  overhead are charged before enqueue, with separately reserved control bytes and slots.
  Default byte ceilings derive from a full supported scheduler batch with finite allocation
  slack; one Add is never split. Disabled pools reject permanently; Full means temporary usage.
  Permits cover queued commands and synchronous consumption; no accounting lock surrounds
  sends, payload drops or wakes. See `driver-admission.md` for ceilings and caller migration.
- **This bounds pending commands only.** Scheduler aggregate storage and outcome-string budgets
  remain separate. Outcome notifications cannot be dropped to enforce command admission.

## 2026-10-17 · Scheduler job-spec storage (#185)

- **Borrowed preflight precedes topology and mutation.** Aggregate forward slots and lane/key
  lengths are checked across existing plus incoming jobs. A separate hard input-capacity policy
  allows ordinary allocation slack; accepted specs use boxed slices and strings, so retained
  accounting has no hidden spare capacity.
- **Reverse edges are unique ordered sets.** Their live entries are bounded by forward slots,
  and requeue no longer scans a wide vector for membership. Ready selection still uses original
  addition order; sibling terminal events follow job-ID order. Forget rebuilds only affected
  surviving dependencies and releases spec charges accurately.
- **Concurrency configuration and outcomes remain separate.** The infallible default
  constructor and public `JobSpec`/`Limits` remain compatible; optional lower storage ceilings
  use a separate constructor. See `scheduler-storage.md` for deliberate defaults and boundaries.

## 2026-10-17 · Provider foundation (#141)

- **`Provider` is dyn-compatible** (boxed futures, `Send` on native only) so the engine can hold
  a registry of `Box<dyn Provider>`; no `async-trait` dependency.
- **`exec` and `providers` don't know each other** (same layer). The engine implements
  `exec::driver::JobRunner` with `providers::run_to_completion`, wrapping exec's `CancelToken`
  and the runtime as `providers::CancelSignal` / `Clock`.
- **A job is submitted once.** Transient poll errors are retried in place. A run that fails after
  the provider accepted the job returns its handle (`RunFailure::handle`), and the engine's
  `jobs::ProviderRunner` keeps it, so the scheduler's retry *resumes* the job instead of
  submitting (and paying for) it again. A job past its deadline is `Expired`, which is never
  retried; a cancel the provider couldn't confirm says so ("may still run and be billed").
  `JobHandle` is serialisable so accepted jobs can be resumed after a restart.
- **Deadlines use real time** (`Clock::now_ms`, the app's monotonic clock), not the sum of sleeps.
- **`submit` may finish at once** (`Submission::Finished`) for synchronous APIs (chat, OpenAI
  Images, TTS), so their outputs don't get stashed in a handle.
- **HTTP through an `Http` trait** so adapters are tested against fixtures. The reqwest transport
  sends only `NodeCraft/<version>` as User-Agent, applies timeouts on every target, caps reply
  sizes while streaming, and keeps header values, URL credentials and queries out of `Debug`
  output and errors. It follows redirects itself: credential headers (the well-known ones plus
  any marked with `secret_header`) are dropped when a redirect leaves the origin, so CDN download
  redirects work without leaking keys. In the browser, `fetch` follows redirects and only drops
  `Authorization`; adapters there must not rely on custom key headers being dropped.
- **TLS:** the OS stack (`native-tls`) on Windows and macOS, rustls with aws-lc on Linux, `fetch`
  in the browser. aws-lc needs clang-cl for Windows ARM64, which the release builds.
- **Downloads are checked and hashed:** sniffed with `media::sniff` (an error page served as 200
  is refused), length-checked against `Content-Length`, and returned with their BLAKE3
  `ContentHash` for the asset store. Inline output bytes go through the same check.

## 2026-10-17 · Plugin system plan (#154)

- **Plan before runtime.** [The plugin RFC](plugins-rfc.md) accepts data-only packages as the first
  distributable scope. Third-party executable actions require a safe-API sandbox feasibility gate;
  no runtime engine, ABI, signature algorithm or marketplace is selected in this documentation PR.
- **One host authority.** Plugin code/data cannot mutate a Session, bypass descriptor validation,
  read raw credentials or submit paid calls outside host quote/budget/idempotency brokers. Schema
  semantics stay with #162; catalog data stays with #77 rather than a plugin-specific variant.
- **Portable preservation precedes execution.** Exact digest locks and bounded missing-plugin
  placeholders retain unknown work. Installation never grants rights or executes project content.
  Browser-unsupported capabilities are explicit; compilation alone does not prove runtime parity.
- **No Rust dynamic ABI.** The MVP avoids native dynamic loading and local unsafe code. Runtime
  selection/resource targets remain measured, reviewed phase deliverables, not shipped guarantees.
## 2026-10-17 · Text action schema and split cardinality (#87, #162)

- **The normative Text inventory lives in `text-actions.md` until #162 supplies the shared
  descriptor contract.** It specifies stable parameter identities, controls, active branches,
  wire defaults and input/output cardinality without introducing a parallel schema runtime.
  Future generated adapters must preserve that contract and its wire fixtures.
- **Split requests persist `max_parts`, defaulting to 16384 for legacy requests.** Explicit
  values are positive and cannot exceed the hard ceiling. Execution intersects this setting
  with the validated host item budget, including a zero host budget; exceeding either returns
  an error without partial output. The setting is an output ceiling, not `splitn` truncation.

## 2026-10-17 · CPU image reference (#85)

- **RGBA8 SDR with explicit linear-light math.** Immutable capacity-checked rasters share a
  private Arc<Vec<u8>>. Exact identity/permutation paths preserve bytes; computed alpha-zero
  pixels are canonical. No implicit HDR clipping or ICC fidelity is claimed.
- **Admission precedes decode/conversion and kernels.** Named input roles, actual capacities,
  inferred output shape, compound work and all worker scratch share bounded preflight.
  Encoded adapters reserve decoder/orientation/RGBA overlap before pixels are decoded.
- **Every named blur is an original finite CPU reference.** Fixed sample order, bounded
  row-ring convolution, explicit parameter minima and deterministic bokeh make native/wasm
  semantics reproducible. Native rayon is an explicit fallible private pool; wasm is serial.
- **The inventory belongs to #162.** Typed action parameters and `image-actions.md` specify the
  normative controls without a second descriptor engine. #88 owns live graph/command adapters;
  #89 owns GPU parity. Split into grid remains #86.

## 2026-10-17 · exec scheduler (#74)

- **The scheduler is a deterministic state machine** (`exec::scheduler`) with no clock or I/O:
  the driver passes `now_ms` in. That makes limits, retries and cancellation property-testable
  with random DAGs and outcomes.
- **Runtime-agnostic driver** (`exec::driver`): the apps implement `Runtime` (spawn, sleep,
  monotonic clock) with tokio or wasm-bindgen-futures; `exec` depends only on `futures`. Futures
  are `Send` on native and not in the browser (`TaskFuture`, `MaybeSendSync`).
- **One scheduler per session, keys per job.** The planner gives every job an idempotency key
  (`nodecraft:<run uuid>:<job>`), used on every attempt so a provider retry after a lost reply
  never creates or charges for a second generation; limits apply across all runs. Finished jobs
  can be forgotten, failed ones requeued, and the queue paused (the budget gate).
- **No scans over all jobs:** ready jobs are queued per lane and backoffs kept in time order, so
  starting, finishing and waking are cheap with 100k jobs.
- **Backoff** doubles from the job's base delay up to its cap, with deterministic jitter of up to
  −25 % so a failed batch doesn't retry in lockstep.
- **Failure and cancellation propagate downstream as `Skipped`/`Cancelled`**; independent branches
  keep running. A failure reported after a cancel request counts as a cancellation (no retry); work
  that finished despite the request counts as done. A panicking runner fails its job instead of
  stalling the queue.

## 2026-10-17 · Programmatic ComfyUI JSON boundaries (#155)

- `ApiWorkflow::to_json`, `PromptRequest::to_json` and `discover` are fallible. Library
  conversion, input checking, mutation and discovery validate borrowed JSON before recursive
  copying or serialization: maximum depth 64 (root depth zero), 2,000,000 values plus object keys,
  and 32 MiB of encoded JSON including escaped strings, keys and outgoing envelope metadata.
  One iterative iterator stack uses at most 65 frames, regardless of array/object width. The
  item cap allows representative 10,000-node API and UI workflows with real sampler parameters;
  it interacts with the 32 MiB and depth caps, so not every 10,000-node combination necessarily
  fits. Large width tests use a private smaller item budget to keep fixtures economical.
- Conversion fan-out, repeated discovery metadata and check-report context share aggregate
  output budgets before retained copies. Accepted JSON is preserved; limits return value-free
  errors attributed to local input, not server responses. Input mismatch errors report types
  rather than arbitrary JSON or selected secrets. Discovery's 32 MiB admission budget includes
  the original workflow and copied exposure data together, so a large literal can fit `to_json`
  but fail `discover`. Only retained combo options are charged: upload combos expose Media
  without copying server input-directory listings, while their copied defaults still count.
  Rejected owned `set_input` values are disposed of iteratively, and mutation validates the
  prospective aggregate before changing the workflow.
- The public structs still expose caller-owned `serde_json::Value`. Calling raw derived
  `Clone`/`PartialEq`, formatting raw Values, or dropping caller-built excessively deep trees is
  the caller's responsibility; the guards protect the library entry points, not those external
  operations. Callers constructing hostile fixtures must dismantle them iteratively too.
  WebSocket progress/output retention (`files_in`) is a separate boundary and follow-up.

## 2026-10-17 · Bounded ComfyUI progress/output retention (#159)

- `PromptProgress::new` and `apply`, and the public `files_in` extractor, now return `Result`.
  The constructor accepts borrowed string-like identifiers (`AsRef<str>`) and validates before
  copying. A failed relevant event preserves the admitted progress/output prefix but latches
  allocation-free terminal `Outcome::MetadataLimit`; the prefix is explicitly incomplete and
  subsequent success cannot overwrite the failure. Oversized terminal error metadata takes the
  same safe failure path. Unrelated prompts and events after completion remain ignored.
- A tracker retains at most 10,000 workflow IDs and 10,000 distinct `(node, FileRef)` output
  associations under an 8 MiB conservative metadata storage budget. Charges include duplicated
  strings, completed-ID reservations and spare Vec/hash-container capacity (256 bytes plus twice
  ID text per workflow node, 1024 bytes plus twice all identifier/file text per association).
  It retains only one current expanded node; only workflow IDs enter the completed set or
  contribute step progress. Exact repeated output associations are idempotent; different nodes,
  filenames, subfolders or kinds remain distinct. These are interacting caps, not a promise
  that 10,000 associations always fit: the byte budget admits fewer than 8,192 even with empty
  strings, and typically about 7,700 with file names, less workflow/current metadata. History's
  256-byte charge can admit all 10,000 files because it retains neither per-file node copies nor
  the live association dedup map; the live tracker's larger accounting is intentional.
- Constructors/events and shallow file extraction examine at most 100,000 entries. Extraction
  caps files at 10,000 and metadata at 8 MiB, charging 256 bytes per file plus all file strings.
  History shares one budget across output nodes/files and retained status/error metadata;
  `HistoryEntry::parse` keeps its signature and can now return a metadata-limit error.
  Cached-ID parser extraction preflights entry count and Vec/String storage before copying
  event fields. `parse_text` propagates extraction errors and moves the parsed raw output rather than cloning
  it to enumerate files. Valid protocol wire data and old-server terminal messages are unchanged.
- Extraction borrows only the output object's immediate arrays and file fields; unrelated nested
  JSON is never traversed or copied. Caller-owned deeply constructed Values still require safe
  caller disposal. Raw event output/preview storage is not retained by the tracker.

## 2026-10-17 · Paused run-driver shutdown (#150)

- Dropping the last run-queue handle drains an unpaused queue normally, including dependencies
  and retries. Closing a paused queue cancels all unfinished jobs instead of resuming potentially
  paid work: queued and retry-waiting jobs end immediately, and running jobs receive their
  cooperative cancellation token. The driver waits for their reports and delivers terminal
  events before returning the scheduler; as with explicit cancellation, successful completion
  despite cancellation still counts as done. Runners must cooperate for shutdown to complete.
- The driver stops polling the command stream after observing closure; only job outcomes and
  retry timers can then make progress.

## 2026-10-17 · ComfyUI protocol layer (#9, part 1)

- **`comfy` is sans-IO.** It builds requests (`Server`) and parses replies and WebSocket
  messages; `providers` owns the transport (reqwest on desktop and wasm, a WebSocket client per
  platform). No async runtime in L1, and every protocol path is tested from fixtures.
- **Clean-room:** written from ComfyUI's public HTTP/WebSocket protocol and JSON formats. ComfyUI
  is GPL-3.0, so its source is never read or copied; fixtures are hand-written.
- **Input order comes from `/object_info`** (`input_order` when present, else document order via
  a small ordered JSON type in `comfy`). The workspace does not enable serde_json's
  `preserve_order`, which would change map ordering, and so hashing, in other crates.
- **UI workflows are converted against a live server's catalog**: widget values are positional.
  Seed "control after generate" values are kept as modes and the upload-button value is skipped;
  reroutes, primitives, Set/Get variable nodes and bypassed nodes (slot by slot) are followed
  (bounded, cycle-safe); notes and muted nodes are dropped. Widget counts that don't line up and
  unknown unconnected nodes become warnings, not silent guesses. Subgraphs and group nodes are
  refused with a clear message for now.
- **Missing custom nodes and models are found before queueing** (`ApiWorkflow::check`), so the
  node shows an error card instead of a server error.

## 2026-10-17 · Instance schemas and provider-ready controls (#53)

- **Checked instance hooks complement static palette descriptors.** Resolve validates bounded
  node config before dynamic code and validates its descriptor before control/port cloning.
  Node-aware modes support workflows and Techniques; pure source planning uses this same path.
- **Configuration is kind-owned, namespaced and versioned.** A separate defaulted `Node.config`
  map retains unknown typed data under graph budgets. SetNode is its mutation path. Explicit
  migration proposals carry warnings and never silently mutate a saved node during planning.
- **Numeric schemas preserve full-width integers.** Optional bounds, unsigned controls, exact
  positive integer steps and seed maxima describe provider constraints without f64 coercion.
- **Hidden values persist but are not execution inputs.** ResolvedParams exposes visible-only
  adapters. All schema text, including large model choice lists and descriptions, shares a cap.

## 2026-10-16 · Settings through craft-settings (#55)

- **App id `nodecraft`** for the folder and `NODECRAFT_CONFIG_DIR`; schema version 1.
- **Portable marker is `portable.txt`** (the Windows portable zip ships one), so the zip tells
  people what the file does; settings go in `config/` beside the executable.
- **Values from the file are hostile:** read them through clamping accessors
  (`CanvasSettings::zoom_speed()`), and an unknown enum value resets only its section.
- **A file with a newer `schema_version` is read but never overwritten** (craft-settings makes
  the store read-only), so running an older build doesn't destroy a newer build's settings.
- **The store lives in the shell** (`NodeCraftApp` ticks it each frame and flushes on exit) and
  is native only; the browser build uses defaults. Changing settings from the UI, CLI or MCP waits
  for the command registry (#52) and goes through a `settings.set` command.
## 2026-10-16 · media: sniffing, decode, thumbnails (#56)

- **Formats are sniffed from bytes, never from file names**, and the sniffed MIME type picks the
  decoder (`ImageReader::set_format`), so a mislabelled file can't reach a different decoder.
- **`image` without default features**, decoding PNG, JPEG, GIF, WebP, BMP and TIFF. No `rayon`
  (wasm) and no AVIF/HEIC decode yet: those are sniffed and reported as unsupported. Animated GIF
  and WebP decode to their first frame.
- **Every decode is capped** (`media::Limits`: 256 MiB input, 16384 px per side, 1 GiB decoder
  allocations by default). The caps are checked from the header before pixels are allocated:
  most `image` decoders ignore `max_alloc`, so `media` reserves the decoded size (plus the copy an
  EXIF rotation makes) against it itself.
- **Images are upright:** EXIF orientation is applied on decode, and `ImageMeta` reports the
  displayed size.
- **`media::ContentHash` is BLAKE3**, matching the asset store's `assets/<blake3>` names. `media`
  can't depend on `types`, so `types`' hash type should convert from its `[u8; 32]`.
- **Thumbnails are RGBA8 with straight alpha**, cached by (content hash, max edge) in a
  memory-budgeted LRU (`ThumbnailCache`). The cache isn't thread-safe; its owner decides where
  thumbnails are made.

## 2026-10-16 · Node kinds and shared parameter schemas (#53)

- **Schema data lives in `types`, re-exported by `nodes`.** Both node kinds and catalog models
  need the same controls; putting the pure data below both L2 crates avoids a lateral dependency.
  Controls cover text, choices, toggles, sliders, integers, colours, model IDs, seeds and assets.
  Schema/default/condition validation runs before any parameter cloning. Conditions reference
  earlier parameters; hidden controllers also hide their dependants, so there are no cycles.
- **Parameters use real scalar values.** `Value` now includes booleans, finite `Number`s, signed
  integers and unsigned integers. Integer values and integer bounds use decimal strings on the
  wire so full-width seeds/counts survive browser JSON. Number construction rejects NaN/infinity;
  workspace `serde_json` enables `float_roundtrip` so defaults and equality conditions survive
  save/load exactly. These variants are part of the initial format; later additions remain
  subject to the format-version gate recorded below.
- **Source planning is pure.** `source.text`, `source.image`, `source.video`, `source.audio`
  accept no inputs and emit existing values/assets on `out`. Empty text is valid; media sources
  may be placed unconfigured but cannot plan until an asset is selected. No placeholder hashes,
  decoding or provider calls occur. Importers/decoders validate bytes; source schemas validate
  the declared MIME modality. Node descriptors expose quick-add and mode compatibility metadata
  for the future palette and Turn-into picker.

## 2026-10-16 · Engine command sessions (#52)

- **Handlers return edits against immutable state.** They perform no I/O; the session applies
  changesets to a draft, validates the undo/redo budget, then publishes the document, history
  and revision event together. Native unwinding is caught before publication. Abort-only wasm
  builds still depend on fallible input validation rather than panic recovery.
- **Drag coalescing is explicit.** A frontend supplies a token unique to one gesture. Only
  consecutive effective moves of the same node on the same canvas with that token coalesce. Undo/redo,
  another edit, a different token or an omitted token starts a new step. The first inverse
  restores the pre-drag position; redo uses the final position.
- **Undo and event history are bounded.** Each undo/redo stack retains at most 256 steps and
  16 MiB of serialized changesets. New edits must fit both their inverse and actual redo
  representation, counting the full changeset envelope and canonical AddNode restore spelling.
  Non-document effects and redundant moves preserve the current gesture and do not publish an
  event or consume a history step. Custom enabled hooks still run on dispatch; builtin graph
  preview validation is deferred to the sole staged transaction. Each consumer holds a revision cursor into the last 256 `DocChanged`
  notifications; an expired cursor explicitly requires a document resync.

## 2026-10-16 · Graph transactions (#51)

- **Stored document maps are private.** Detached node data is editable; a stored document changes
  only through serializable changesets. Deserializing a project runs structural validation.
- **Changesets are ordered and atomic.** Existence, payload and count checks apply per edit;
  relationships are checked once at commit. Temporary cycles, occupied input orders or missing
  field/tag references can be repaired within the batch. New edges require existing nodes,
  and carried snapshots must pass payload validation. Staging one clone keeps failed edits from
  partially mutating the live document. Inverses use the same bounded path as forward edits.
- **History edits are granular.** Indexed generation insertion/removal and active selection
  preserve history order without serializing the whole node in every undo record.
- **Aggregate JSON budgets cover both directions.** Nodes are capped at 8 MiB, changesets and
  their generated inverses at 64 MiB, and projects at 256 MiB. Reject an oversized undo before
  publishing its forward edit. These limits and graph/journal variants belong to the file
  version contract; check that version before deserializing and overwriting a project.
- **Connections form a DAG.** Ports must have matching modalities, a list output cannot feed a
  single input, and each input order is unique. Cycles are rejected with an iterative walk.
- **Node deletion has a compact inverse.** One restore operation reinstates the node and its
  incident edges, avoiding an undo operation-count overflow for highly connected nodes.

## 2026-10-16 · Core types (#50)

- **Entity IDs are typed UUIDs supplied by the caller.** The pure-data layer has no random
  number generator or platform dependency. Serialized UUID strings also work as JSON map keys.
- **Money uses integer millionths plus a currency code.** Arithmetic is checked and rejects
  mixed currencies. The amount is a decimal string on the wire, avoiding loss in browser JSON
  clients above the exact-integer range of floating-point numbers.
- **Fractional prices are aggregate rates, rounded once after usage multiplication.** Keep a
  $0.075-per-million-token price as 75,000 micros per million, then use `checked_mul_ratio` with
  total usage and a denominator of one million. Its u128 intermediate avoids premature
  rounding and overflow. Budget quotes round up, so a non-zero fractional cost cannot bypass
  a budget; settled amounts may explicitly round down where the provider contract specifies it.
- **Unknown error categories preserve their message as `ErrorKind::Unknown`.** Unknown
  `Modality` and `Value` variants deliberately fail decoding: new variants require a `.ncraft`
  format-version gate before graph deserialization. The format layer must reject unsupported
  versions with an actionable update/compatibility error, never silently discard unknown data.
- **MIME essence selects decoders.** `Mime::essence()` compares normalized type/subtype;
  full equality retains parameter spelling and spacing rather than promising semantic equality.
- **Asset hashes and MIME values validate on deserialization.** Media metadata uses optional
  integers; resource limits belong to the decoder consuming those measurements. `Value` keeps
  nested lists, so project readers must retain their decoder recursion and input-size limits.

## 2026-10-16 · M0 skeleton

- **Layer numbering follows the plan (L0–L5), not PhotoCraft's L0–L7.** The rule "only the shell
  uses UI crates" is an allow-list (`ui-egui`), because `agent` and `automation` share the shell's
  layer but must stay headless.
- **`media` is standalone** (no workspace deps). `graph` depends on it, not the other way round.
- **Empty crates already declare their planned workspace dependencies**, so the layering check is
  meaningful from day one and the dependency graph is visible in `cargo xtask layers`.
- **craft-libs pinned at `8f5a283`.** `craft-ui` and `craft-quit` are used by the shell now;
  `craft-settings`, `craft-atomic` and `craft-pdf` are pinned in `[workspace.dependencies]` but
  unused until M1/M8. Note that `craft-pdf` relies on `[patch.crates-io]` entries in craft-libs'
  own workspace (vendored `krilla`/`hayro`), which do not apply to a git dependency; check this
  when `craft-pdf` is first used.
- **`ui-egui` depends on eframe with `wgpu`** (not only egui) so the `eframe::App` impl lives in
  the shell and `cargo test -p nodecraft-ui-egui` builds on its own; eframe without a renderer
  feature does not compile on macOS.
- **Canvas navigation:** scroll pans and pinch/⌘-scroll zooms (trackpad-first, like the reference
  products); see `ui-design.md`.
- **Releases:** our own `release.yml`, modelled on the shared Craft release workflow, because the
  shared one fails on the `release` branch without Apple and Windows signing secrets and we have
  none yet (#100). Ours signs when secrets exist and otherwise ships unsigned builds with
  first-launch instructions (`docs/install.md`). It is written from scratch (craft-common-actions
  has no licence file, so its text isn't copied). Targets: macOS universal, Windows x64/arm64,
  Linux x86_64/aarch64 + Flatpak; the web job comes with `apps/nodecraft-web` (#99). Switch back
  to the shared workflow once signing is in place, if it is still the better fit.

## 2026-10-17 · Run planner (#73)

- **The planner plans nodes, not only jobs.** `NodeKind::plan` needs concrete input values, but a
  node fed by another step of the same run has none until that step finishes. A `RunPlan` is
  therefore a list of `Step`s in dependency order, each with its inputs (an existing value, or a
  named output of an earlier step) and its `JobSpec`s when every input is already known; the
  driver plans the rest as their upstream steps finish. The scheduler (#74) receives job
  dependencies derived from the steps' `after` lists.
- **Source nodes are resolved inline.** An upstream source outside the run contributes its content
  directly (its `plan` is free and deterministic), so *Run selected* never blocks on an unrun
  text or image source; a source only becomes a step when it is requested itself.
- **Staleness is an input.** The planner takes the stale set from its caller; computing it from
  input hashes is #75. *Run all stale* plans the stale nodes plus the empty or stale ancestors
  they need, like *Run with upstream*.
- **List inputs are planned as one `Value::List`** per port, in noodle order; single inputs pass
  their one value. A list *output* wired into a list input is **spliced** (its items join the
  list) rather than nested; `InputSource::Step { list: true }` tells the driver to do the same.
- **Staleness is transitive by contract.** *Run with upstream* stops at an ancestor with a fresh
  result, so the stale set handed in must already contain everything downstream of a change.
- **Who mints job ids.** The driver turns steps into scheduler jobs (`nodecraft-exec`'s own
  `scheduler::JobSpec`, distinct from the node-level `nodecraft_nodes::JobSpec`): it mints job ids
  and `nodecraft:<run>:<job>` idempotency keys, and gives every job of a step a dependency on
  every job of the steps in its `after` list. Steps planned later are added as placeholders so a
  failed upstream can skip them and the queue panel can show them.
- **Pricing is not the planner's job.** A `RunPlan` carries no cost; `cost::quote` (#76) prices
  its steps from their nodes' models and parameters, which don't depend on upstream values, so
  multi-step runs can be quoted in full before they start.
- **Planning is bounded** for hostile projects: upstream values are resolved once per output,
  one list input holds at most `MAX_VALUE_ITEMS` items, one step's known inputs at most 64 MiB
  and the whole plan's at most 256 MiB, and at most 256 blockers are reported; past a limit the
  run is blocked with a reason rather than planned.

## 2026-10-17 · Model catalog (#77)

- **Catalog files are TOML** (`assets/catalog/<provider>.toml`), embedded in the build so they
  work offline and on the web, parsed with `toml` (parse + serde only). Comments and nested
  tables read better than JSON for hand-edited data.
- **The shipped catalog prices nothing yet.** It holds free local ComfyUI-backed models. Priced
  cloud entries land with their provider adapters, whose PRs say where the prices came from:
  a guessed price would mislead people about what a run costs.
- **Prompts are an inline input role.** A media node's own prompt field fills `inline` text
  roles, so a fresh node with nothing wired still offers text-to-image models; a wired text
  input replaces the inline prompt.
- **Quotes are computed from resolved parameters** (schema defaults applied) and round up, and a
  per-unit model has exactly one counted parameter; anything else is rejected when the catalog
  loads rather than under-quoted later.
- **`catalog-check` is an integration test** of `nodecraft-catalog` that `cargo xtask
  catalog-check` runs, so xtask stays std-only and `cargo test` covers it too.

## 2026-10-17 · Cost quotes and budgets (#76)

- **Pricing lives in `cost::quote`.** A node is paid when its kind has a model parameter
  (`ParamControl::Model`); its price is the sum of the catalog models its model parameters name
  (each priced from its own parameters via `model_params`, which provider requests must use too)
  times the jobs the step plans. A paid step whose jobs are planned later is quoted as one job and
  flagged as an estimate (`Quote::estimated`, so the total is not "complete"); the driver checks
  budgets again before each job (#207). Kinds without a model parameter
  (sources, local actions) are free, except generation kinds: one with no model to price is
  unpriced, never free (#219). Category is a stand-in signal: local generation kinds (a ComfyUI
  workflow) will need to declare that they are free, or capped runs with them will be blocked. Totals are kept per currency and never converted.
- **Caps are conservative.** A capped run is blocked when any step's price is unknown (no model,
  model not in the catalog, usage-based pricing without a count) or is in a different currency
  from the cap; spend in other currencies doesn't count towards a cap. The per-call cap applies
  to each job; session and project caps to what was already spent plus the run. Amounts equal to
  a cap are allowed, and a run that costs nothing is never blocked.
- **The usage log counts a charge once per provider and idempotency key** (empty keys are refused), keeps the
  newest 100,000 entries while folding evicted spend into running totals so caps never loosen,
  tracks sessions by id, and exports CSV that spreadsheets can't read as formulas.
- **Totals are kept as charges arrive and saved with the log** (#219). `spent()` can't fail, a
  charge over a million units is refused, and nothing changes on an error. A saved log keeps its
  totals (`UsageLogFile`), so reopening a project never forgets evicted spend. Loading checks
  that the totals cover every listed charge.
- **Charges have a status** (`Charge`: actual, estimate, unresolved). `settle` moves an estimate
  to the actual cost, or keeps it as unresolved when a job may have been billed. Totals move by
  the difference; an actual charge is final. Recording the same key again never replaces a
  charge, so a retry can't change what was billed. `release` removes an unsettled charge the provider certainly
  never billed and frees its key. Settling can't change a charge's currency. A saved log's
  project total must equal the sum of its sessions and stay far below overflow.

## 2026-10-17 · Staleness (#75)

- **A result records the hash of what it was made from** (`Generation.input_hash`, optional so
  existing results load; they are never reported stale). The driver stamps it with
  `exec::staleness::input_hash` when it adds a generation; `.ncraft` archives must carry the
  field (#134).
- **What is hashed:** the node's kind, its configuration, its *visible* resolved parameters (a node
  its kind rejects has no hash and counts as unknown), and per input port in
  noodle order an upstream source's content or the id of an upstream node's active result.
  Positions, titles, tags and unrelated nodes don't count. A domain/version tag is mixed in, so a
  change to the recipe makes old hashes unknown rather than wrongly fresh.
- **Stale is transitive:** a result is stale when its hash no longer matches *or* any upstream
  node is stale (it was built on out-of-date data). This is the set `planner::plan` expects, and
  `canvas.stale` returns it read-only, using the session's authoritative node-kind registry
  from Context.kinds, shared with node editing and rendering. Session::new supplies builtins;
  with_registries supplies custom kinds without a second registry in Resources.

## 2026-10-17 · Router node (#72)

- **`structure.router` is a node kind with no work of its own:** one list input and one single
  output, in a modality chosen per instance (`config["modality"]`, `text` by default), so it can
  feed any input of that modality. Quick-add `R`. Renaming uses the node title.
- **Routers are resolved away in one place, `exec::routing::incoming`:** every consumer's feeds
  through routers (and chains of routers) become the routers' own sources, in order. The planner
  and staleness both read inputs through it, so a router adds no step, no cost and no hash, and
  editing what feeds a router makes its consumers stale.
- **What a router can't hide is blocked with a reason:** a single input fed several values, or a
  list, through a router; and an input wired only to routers with nothing behind them (it isn't
  treated as unconnected, which could silently pick another mode). Routers in a run request are
  dropped (selecting only routers is "nothing to run").
- **Resolution is linear and bounded:** each router's sources are resolved once, routers before
  the routers they feed (Kahn's order), each list capped one past the list limit (so the planner
  still reports an oversize fan-in), and at most 4M entries in total; past that, resolution is
  refused. No recursion, so long router chains can't overflow the stack.

## 2026-10-17 · Router validation (#216)

- **Resolution uses the installed registry.** Each router instance is validated once against
  its configuration and stored ports before it can act as a passthrough. A bounded, fixed-size
  reason and the first invalid router identity travel through router chains to each affected
  consumer port. Valid feeds on that port cannot hide an invalid branch; unrelated selected
  nodes can still plan. Original wired ports are indexed once alongside incoming edges, so an
  empty router remains distinct from an unconnected input without rescanning edges per port.
- **Unknown is not fresh.** An invalid-router-dependent node cannot produce an input hash and
  remains unknown under the existing staleness contract. The stale set lists known changed
  results, not proof of freshness; callers needing that distinction use `input_hash`. Global
  resolution budget failures return errors from both `staleness::stale` and `canvas.stale`,
  without changing the document or undo history. This does not change hash bytes or versions.
## 2026-10-17 · Graph-backed canvas cards (#57)

- **An empty stored title means automatic.** New descriptors leave titles empty. The shell derives
  a bounded label from the prompt/text or kind label; edits and undo retain the original graph data.
- **Schema edits are engine commands.** The session owns an injectable node-kind registry so the
  canvas and programmatic callers validate against the same descriptors. Null resets an override;
  missing values are malformed requests. Unknown kinds remain movable and renameable.
- **Widget drafts are view state.** Text commits on blur/submit, sliders on release, and Escape
  cancels text edits. Failed drafts survive changes to other fields. Card motion uses a unique
  engine gesture token, preserving one undo step across pointer events at every zoom level.

## 2026-10-17 · Bounded graph-card editing (#57, #145)

- Full prompt data is retained; idle cards borrow values and lay out bounded previews. Full drafts
  are lazy, and schema/visibility caches follow document revisions. Dirty/invalid drafts survive
  unrelated undo/redo. Hidden dirty editors explicitly advertise unsaved data and offer discard.
- A drag previews position in shell view state and dispatches one node.move on release. Typed
  slider changes wait for Enter/blur; redundant numeric changes do not create history entries.
- Escape cancellation remembers ownership across egui's focus transition (#145). A failing-before
  event regression verifies the old has_focus-only condition committed; the fixed test preserves
  graph and undo state through the following frame.
- Overview labels remain readable, controls appear only at readable scale, and named keyboard
  Rename actions complement double-click. Settings store lifecycle and scroll preferences remain
  intact in the graph-backed shell. Selection expansion remains owned by #58/PR144.

## 2026-10-17 · Staleness hashes content, within a budget (#213)

- **Upstream results count by content, not id.** A consumer's input hash covers the digest of
  the upstream active result's output on the port it reads. Result ids aren't immutable: `SetNode`
  can rewrite a result's outputs under the same id, or it can be removed and re-added. With
  content hashing, that makes downstream results stale, while an identical re-run upstream keeps
  them fresh. A result recorded for another port layout counts as "empty", as the planner
  treats it.
- **One query, bounded work.** Each node's recipe and each upstream output is digested once per
  query (memoized). Values are streamed into the hasher instead of being serialized. Every byte
  is charged against a 256 MiB budget (`staleness::MAX_QUERY_BYTES`), and a canvas past it gets an
  actionable error from `stale` and `canvas.stale`.
- **Older recipes read as stale, deliberately.** The stored hash is still a bare Blake3 (no
  format change), and its domain tag moved to `v2`. A result stamped under another tag can't be
  told from one whose inputs changed, so it is reported stale: we never show a result as fresh
  when we can't vouch for it. The producer that stamps hashes isn't wired yet, so no saved
  project carries `v1` hashes.
- **`input_hash` is a new semantic field in saved generations.** Archive and journal versions
  that accept it, and the guards that make old readers reject it, are coordinated in #134 and
  #162. This change doesn't alter the stored field.
- This supersedes the #75 entry's "identity of an upstream node's active result" and "old hashes
  become unknown".

## 2026-10-17 · Cost admission and usage-log retention (#214)

- **A guess is admitted provisionally.** `cost::check` returns an `Admission`. When a session or
  project cap is set, paid steps quoted at one job before their job count is known are listed as
  `provisional`: the run may start, but the check didn't cover their real cost. The guarantee is
  job-time enforcement in `engine::jobs` (#207, #222), which checks each job as it starts. A known
  empty job list quotes zero and blocks nothing.
- **Charge identity outlives the list.** When an entry leaves the list, a 16-byte digest of its
  provider and key is kept (saved with the log, up to 250,000 of them), so a late retry of it is
  still the same charge. The boundary is explicit: a retry of a charge more than
  `MAX_USAGE_ENTRIES + MAX_RETIRED_KEYS` charges old counts again.
- **Retained bytes are bounded.** Listed entries hold at most 32 MiB of text, and the oldest
  leave first, as at the entry limit. With long fields that is about 16,000 entries.
- **Open charges stay settleable (#228).** An open charge (an estimate or an unresolved one)
  pushed off the list is kept as *pending*, up to 10,000 of them and saved with the log. It can
  still be settled, released or found with `charge`. Once settled, only its identity is kept.
  Past the limit, it stays charged as it is, which is conservative. `settle` enforces the same
  total ceiling as `record`. At most 1,024 sessions keep their own totals. The least recently charged one folds into an
  "earlier sessions" total saved under the reserved empty name. Two kinds of session never fold:
  the one charging, and any with an open charge (#233). Otherwise a reused session id would
  settle an old charge out of its new total and undercount. Sessions kept for open charges can
  exceed the limit, by at most the number of open charges, until the next new session folds
  them; loading allows that. The session
  clock forgets folded names, so it stays as small as the session map. Settling or releasing a
  folded session's entries adjusts the earlier-sessions total, so the project total and the
  current session's stay exact and a saved log always reloads. (Logs saved before #233 can hold an open charge whose
  session already folded; if that session id charges again, the old undercount can recur. Session
  ids are per launch, so this needs an old log and a reused id.) A session id that charges again after
  being folded starts a fresh session total. Entries with an empty session id were never written
  by NodeCraft and are now refused. The CSV export can stream (`write_csv`).

## 2026-10-17 · Run executor (#231, #210)

- **`exec::run::Run` drives a `RunPlan` to results.** It is a pure, synchronous state machine
  that borrows nothing. The host passes the node kinds to each call, feeds it driver events
  (`job_done`, `job_failed`), adds the jobs it returns and cancels the ones in
  `Progress::cancel`: the unfinished jobs of a step that failed. Steps start in dependency order,
  tracked per step (Kahn's algorithm, linear in the plan). A plan whose steps aren't in that
  order is refused, which also rules out cycles. A step after a failed one fails as "skipped".
- **Deferred steps are planned from the run's own outputs**, by calling `NodeKind::plan` once
  their predecessors finish. List outputs are spliced into list inputs. `RunLimits` apply before
  anything is copied; they default to the planner's 4,096 items per list input and 64 MiB per
  step, plus 256 MiB per run for inputs materialised at run time (#210). A step may plan at most
  4,096 jobs and a run 100,000.
- **Step outputs are positional.** A job reports `(output port, value)` pairs. A list port
  collects its values in job order; a single port must get exactly one value, or the step fails.
- **Results are stamped with the inputs actually used.** The run keeps the canvas as planned,
  with routers resolved once, and an `staleness::InputHasher` that memoizes digests. A step's
  `input_hash` is computed with the run's own upstream outputs standing in for those nodes'
  results. It matches what `staleness` computes once the results are recorded, and edits the
  user makes mid-run don't change what the run hashes.
- **Generation ids and keys are deterministic per run.** A generation id is derived from the
  run id and the node; a key is `nodecraft:<run hex>:<job>`, which is ASCII and under 64 bytes.
  Job ids come from the host, which keeps them unique in its scheduler.
- **The engine records a result** with `record_changes`, given the live node: `AddGeneration`
  plus `SetActiveGeneration` as one undo step per result. To make room, it drops the oldest
  inactive generations, both at the 1,024-entry history limit and until the node fits its 8 MiB
  limit. A result too large for any node is reported as unrecordable, never silently dropped.
- `SourceRunner` runs `JobSpec::Source` jobs. Provider jobs plug in through the host's runner
  via `Run::work`.

## 2026-10-17 · Submission certainty (#227)

- **The provider task decides whether a failed submit may have been billed**
  (`providers::RunFailure::uncertain`), because only it knows whether the submit was sent. Any
  of these is uncertain: a submit interrupted mid-flight (cancelled while submitting, timed out,
  cut off, a server error), or a reply that can't be read or says the job ran (`Malformed`,
  `JobFailed`, `Expired`). Job-time budgets (#222) keep those reservations and charge them as
  `unresolved`. A run cancelled before its submit, and refusals (`Auth`, `UnsupportedInput`,
  `BlockedContent`, `Budget`, `RateLimited`, `Busy`), are certain, so their reservations are
  released.

## 2026-10-17 · Noodles (#59)

- **Multi-change canvas edits are their own commands.** Replacing the noodle on a fed single
  input, rerouting one, and deleting a selection must each be one undo step and all-or-nothing,
  so `edge.rewire` (remove up to two edges, connect one) and `canvas.delete` (edges, then nodes)
  build a single changeset instead of the shell dispatching several commands.
- **The shell makes edge ids by hashing** the endpoints, the session revision and a probe counter,
  and checks them against every canvas. `types` takes no randomness and the shell has no portable
  random source on wasm yet. CRDT sync (#119) needs peer-unique ids and should replace this with
  a platform random source.
- **Inputs on the left, outputs on the right**, spread evenly down the card edge; a single port
  sits at the middle, which keeps the existing `+` handle where it was.

## 2026-10-17 · Failed deferred input materialization (#244)

- **Preflight the whole deferred step before copying its inputs.** Borrowed source slices check
  availability, cardinality, list counts and checked byte totals first. A late invalid input or
  limit refusal therefore copies no accepted prefix and consumes no cumulative copy allowance.
  Splicing does not allocate a temporary list of references. Runtime logical-size accounting
  matches the planner, using a depth-bounded iterator stack and document depth/item limits
  before recursive clones. Sources cannot change between the preflight and copy passes in
  the synchronous executor.
- **Admitted copies remain charged when planning fails.** Once the complete input copy is
  admitted, its bytes count toward `RunLimits::run_bytes` even if the node kind subsequently
  refuses to plan. Independent steps may use only the remaining allowance; existing per-step
  and per-list limits, ordering and failure propagation remain in force.

## 2026-10-17 · Live canvas navigation (#62)

- Navigation stays shell view state. Stable engine commands produce bounded service intents, with
  no graph changes, history or events. The app bridge supplies a fresh scoped capability and
  restores any previous resource even on dispatch failure; raw headless dispatch is Unsupported.
- Fit and minimap geometry resolve real instances once per revision and retain only bounded node
  rectangles. They never rely on the evictable visible-card schema cache or static palette metadata.
  The minimum zoom makes very spread graphs best effort, with an explicit notice.
- Shift+1/2 fit all/selected; Shift+M toggles the map without macOS's Command+M conflict. Editors,
  popups and existing gestures own their input. A minimap drag freezes its world mapping and owns
  pointer release outside its bounds. Visibility is session-local, not serialized settings.

## 2026-10-17 · Provider identity and unreserved costs in budgeted jobs (#228)

- **Handles belong to the provider that issued them.** The runner keys stored handles by
  provider and idempotency key, and `restore_handle` takes the provider. A job moved to another
  provider under the same key never resumes the first one's handle. Its fresh submission is held
  to the caps, and the old attempt's reservation is closed first, as `unresolved` if it may have
  been billed.
- **A reported cost counts even with nothing reserved.** A priced job with no estimate, running
  with no caps, reserves nothing. When it finishes, fresh or resumed, the cost the provider
  reported is recorded as `actual`, once per provider and key. A report the log refuses as
  implausibly large is left out rather than failing the finished job.

## 2026-10-17 · Quick-add and palette (#61)

- **`node.create {canvas, id, kind, pos}`** creates a kind with its default parameters through
  the node registry, so quick-add keys, the palette and agents share one command; `node.add`
  stays the low-level "insert this exact node". Ids are supplied by the caller (`crate::ids` in
  the shell hashes and probes, as for edges).
- **Quick-add keys come from node kinds' `quick_add` metadata**, not from hard-coded handlers.
  A binding is one ASCII letter or digit, optionally `Shift+` (so `Shift+T` for the technique
  picker works later); anything else is not bound. They fire on a fresh press (not key repeat)
  with exactly those modifiers, outside text fields, menus and gestures, and never while the
  palette is open (the palette also takes canvas keys like Delete and Escape for itself).
- **The palette lists commands that are enabled with no parameters right now** (e.g. Undo when
  there is something to undo); commands that need a target are reached from the canvas or menus.

## 2026-10-17 · Reservation ownership across re-registration (#263)

- A provider/key replacement closes the previous reservation even when the replacement has no
  JobPrice or is cancelled before submission. A previous in-flight submit may already be billed
  before a handle arrives, so its estimate becomes unresolved. Unpriced work still adds no charge.
- Submitted reports, completion/failure and dropped-future accounting affect only their captured
  provider/key. Running ownership also follows that identity, so an old guard cannot consume a
  replacement's deferred settle/forget or release its pending charge. Matching retries retain
  their reservation and ordinary cap enforcement. This does not redesign provider-handle lifecycle
  or authenticate overlapping invocations with the same provider/key.

## 2026-10-17 · Copy, paste, duplicate (#63)

- **The clipboard carries versioned JSON** (`{"format":"nodecraft.nodes","version":1,…}`) of the
  selected nodes and the edges between them, so copies move between projects and windows through
  the OS clipboard. Paste remaps every id, drops edges to nodes that weren't copied, and lands
  the group centred on the pointer, as one `canvas.paste` changeset.
- **Copies leave generation results behind** (`node.add`'s rule): pasted nodes start without
  history. Copying results waits for the asset store (#54); pasted images and dropped files are
  #197.
- **Pasted nodes of installed kinds must match them** (ports as the instance declares, parameters
  the schema accepts); nodes of kinds not installed here are kept as data, as a loaded project
  keeps them. Tags and fields the target canvas lacks are dropped, so copies move between canvases
  and projects. A NodeCraft copy this version can't read shows a notice instead of becoming text.
- **Parameters travel through the OS clipboard as they are**, which is safe because kinds never
  keep secrets in parameters or config (keys live in the keychain).

## 2026-10-17 · Model calls as jobs, the mock provider and golden graphs (#79)

- **`JobSpec::Generate` is the one way a node calls a model.** It names a provider adapter by id,
  a model, the node's parameters and role-tagged inputs, plus the output port the results go to.
  The provider id is also the scheduler lane, so per-provider limits apply. `exec::run` treats it
  like any job; `SourceRunner` refuses it.
- **`engine::generate` translates in both directions.** Parameters go to the provider as plain
  JSON. Inputs go as role-tagged text, with a list sending one entry per item. Media inputs are
  refused as unsupported until the asset store exists. A `Completed` result comes back as text
  values, or as media values that reference the returned bytes by Blake3 hash; the host stores
  the bytes. URL outputs must be downloaded first.
- **`nodecraft-testkit` provides `MockProvider`**, a deterministic fake for text, image, video and
  audio models. Its results depend only on model, parameters and inputs. It can be scripted per
  model: polls before done, a reported cost, failure at submit, poll or job. It records requests
  and bills once per idempotency key. It also provides mock generation kinds (`mock.text`,
  `mock.image`).
- **Golden graphs run a canvas end to end:** plan, run executor, `run_to_completion` against the
  mock, results recorded with `record_changes`. Each step's outputs are compared with a blessed
  JSON file, and the project itself with a second one, so serialization drift shows up in review.
  Each golden also checks that nothing is stale after its results are recorded. Bless with
  `NODECRAFT_BLESS=1`. The project files are graph JSON until the `.ncraft` archive (#134)
  lands.
