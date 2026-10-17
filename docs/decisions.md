# Decisions

Decisions others will build on, newest first. The product-level ones are in
[`plan.md`](plan.md) §6; this log records the ones taken while building. Add an entry when you make
a choice a later contributor might otherwise undo.

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

## 2026-10-17 · Graph-backed canvas cards (#57)

- **An empty stored title means automatic.** New descriptors leave titles empty. The shell derives
  a bounded label from the prompt/text or kind label; edits and undo retain the original graph data.
- **Schema edits are engine commands.** The session owns an injectable node-kind registry so the
  canvas and programmatic callers validate against the same descriptors. Null resets an override;
  missing values are malformed requests. Unknown kinds remain movable and renameable.
- **Widget drafts are view state.** Text commits on blur/submit, sliders on release, and Escape
  cancels text edits. Failed drafts survive changes to other fields. Card motion uses a unique
  engine gesture token, preserving one undo step across pointer events at every zoom level.

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

## 2026-10-17 · Incremental canvas search (#69)

- Search indexes persisted Text leaves and linked tag titles, with checked actual title fallbacks.
  Stored model identifiers match today; catalog display names wait for verified catalog integration.
  Unicode scalar lowercase literal matching is explicit, without normalization/full case folding.
- Work is incremental over borrowed text and bounded structural cursors. Only IDs, query/matcher
  state and bounded fallback labels survive; no full prompt/lowercase copies. A pending step belongs
  to its query generation and waits for complete results before wrapping. Idle frames do no scans.
- Search commands use a scoped live host capability, retaining document/history/event identity.
  Only the exact search editor's current/prior focus permits framing; unrelated editors, menus and
  gestures retain ownership. Opening search never steals an unrelated dirty editor's focus.

## Selected-node auto-format geometry and ordering (#70)

`canvas.auto_format` is an atomic edit, not a host service mutation: one invertible MoveNode
changeset, no gesture coalescing, and no history/event entry for an unchanged result. A scoped
`LayoutContext` supplies selection and current logical geometry. Engine Context now exposes the
Session revision; project/canvas/revision and exact selection membership are checked before
layout. Custom hosts are trusted to compute view metrics at their zoom and must refresh the
capability after every document revision. Live `dispatch_layout`/`layout_enabled` build it afresh
and restore any prior capability on success or error. Raw headless sessions without this explicit
capability return Unsupported. No dialogs are opened by command dispatch.

Keep two logical size scalars per actual resolved node alongside the existing checked geometry
cache (at most 10,000 nodes, 80,000 bytes of scalar payload, plus bounded map overhead). Compute
size while borrowing the validated instance descriptor, never from large f32 Rect differences.
No new descriptor bodies or instance hooks are retained/performed for formatting. Shared card
metrics include the clipped title/UNSAVED notice, minimum screen-sized handles and half-stroke.

Use nearest-8 outer y/x keys (ties away from zero), then NodeId; align anchor and cell spans to
8 world points. Positive left/top decoration insets are added to cell origins. This makes reading
order stable under mixed insets and f32 conversion across the legal coordinate domain. Reject
host metrics with an outer span above 1,000,000 points and reject all output when any body
position exceeds ±10,000,000; never clamp individual results. Reapplication assumes unchanged
logical geometry/topology/zoom, rather than pretending a position-dependent custom kind cannot
change its own size. Whole-DAG longest-path ranking uses a stable ordered ready set in
O((V+E) log V), avoiding repeated transitive searches. Only selected ranks are placed; obstacles
and edge-crossing optimization are outside this contract.


## 2026-10-16 · Project archives and recovery (#54)

- **Stored ZIP with a narrow, versioned surface.** Format v1 accepts only a bounded manifest and
  canonical content-addressed assets. Metadata is checked before library parsing; no archive path
  is extracted. CRC and BLAKE3 checks catch both transport corruption and content-name mismatch.
  Pure codec and recovery replay run on wasm; native I/O stays behind `ProjectStorage`.
- **One publication boundary per operation.** Graph edits are staged, journaled, then published.
  Opening is an explicit session lifecycle replacement after complete validation, not a synthetic
  enormous undo transaction. Saving publishes a file before changing the session savepoint.
- **Dirty follows content state, not notification revision.** Undo entries restore state IDs;
  saves break coalescing so a drag cannot swallow a saved boundary. Status has its own cursor.
- **Recovery is append-only between checkpoints.** Checksummed sequential frames carry actual
  forward changesets and newly referenced asset bytes. A torn/checksum-bad final frame or zero-filled
  tail recovers the validated prefix with truncated_tail; corruption before later data remains an
  error. Native append handles stay open and a worker group-syncs every 250 ms (plus scheduling/I/O
  latency); the latest window may be lost on power failure. Save/clean-close flush explicitly.
  There is no synchronous fsync for every drag move. Worker failures latch until a new checkpoint. Checkpoint identities include an advancing
  epoch; stale frames cannot replay onto a new checkpoint. Failed appends latch editing until save.
  Recovered snapshots are explicitly untitled and dirty to avoid overwriting a newer destination.
- **Publication and durability differ.** `craft-atomic` reports a rename as successful even if
  directory sync fails afterward. We retain that contract. If subsequent recovery checkpointing
  fails, save still reports published and separately reports recovery unavailable.
- **Initial limits are conservative.** Archives cap at 64 MiB with 48 MiB aggregate media; buffering
  and shared immutable assets keep resource behavior explicit. Lazy disk-backed large projects
  need a streaming storage implementation before these limits can safely grow.

- **Clean close and asset lifetime are explicit.** Hosts flush/discard recovery after saving or
  an explicit dirty discard; dirty/crashed sessions retain recovery. Save-time GC retains live
  and undo/redo asset references. Canonical destination locks prevent silent concurrent native
  saves; lock retention is bounded to 64 destinations per storage lifetime.
- **Container checks are not media decoders.** Known signatures and dimensions from available image probes are
  checked; audio/video/document metadata needs later probes. Project hashes use types::Blake3,
  bridged to media::ContentHash at format. The bounded initial codec still needs a separately
  tracked file-backed/lazy asset implementation to meet plan 4.6 for large projects; see [lazy asset storage #147](https://github.com/CypherNaught-0x/nodecraft/issues/147), dependent on #54.


## 2026-10-17 · Local discussions, notes and format v2 (#68)

- Keep legacy root text and add defaulted flat replies, resolved state and optional node-relative
  anchors. Node deletion detaches discussions instead of deleting their content; undo stores only
  pin IDs/offsets beside the restored node/edges.
- Preserve valid-final transaction ordering. Structural batches with possible discussions certify
  A→B→A→B and identical inverse spelling, preflighting actual redo under graph/history/journal
  caps. One extra clone/two probe applications are an explicit bounded tradeoff; move-only edits
  stay on their existing single-stage path.
- Retain 8 draft allocations/2 MiB String capacity with exact held Session identity and revision.
  Blur never saves; explicit Save/Cancel and document undo avoid retaining egui's 100 full-text
  draft snapshots. Inactive previews are bounded; focused editing still has transient egui copies.
- Notes use structure.note with no ports/jobs, not an executable source-text alias. Local-owner
  editing has no fabricated account/ACL metadata; #121 owns future central sharing permissions.
- Freeze Comments archive v2/NCRJ0002 with exact reviewed-v1 fixtures, original checkpoint-byte
  recovery identities and atomic conversion before new appends. Scalar Values and Node.config
  are v2 additions too. Later Tags/Fields integrate as v3 with lossless v1/v2 defaults; old headers
  reject their actual reserved roles even when values are null/default, without rejecting harmless
  unrelated extension objects. This avoids competing incompatible v2 writers.

## 2026-10-17 · Persisted Tags/Fields compatibility bridge (#193)

- Introduce archive v3 and NCRJ0003 before enabling Tags/Fields command and UI branches.
  Frozen v1/v2 contextual guards remain unchanged. Both older archive versions upgrade to a
  v3 checkpoint before adoption/append; journal replay authenticates original archive bytes.
- Persist Field rectangle/color/collapse and Tag color, plus compact SetNodeTags, SetNodeFields
  and absolute MoveNodes. Legacy fields default to (0,0,320,240), no color, expanded; membership
  remains explicit node data even outside the zone. Total bulk occurrences are capped at10000
  across the batch and inverse, independently of Comments' pin metadata and byte budgets.
- Preserve Comments' certified forward/inverse/actual-redo staging, relative pins and final
  world-position validation. Compatibility lands from tag-free Comments before feature UI,
  so no intermediate writer emits data rejected by its own declared version.
