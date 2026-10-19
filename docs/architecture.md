# Architecture

> The design is in [`plan.md`](plan.md) §3–4; this page is the map of what exists in the tree and
> the rules the tree enforces. Keep it in step with `xtask/src/layers.rs`.

## Principles

The engine comes first and the GUI is one client of it. Every user-visible action is a command
with a stable id that the UI, CLI, control channel, MCP server and in-canvas agent all dispatch.
Commands change the document only through invertible, serialisable changesets, so undo, autosave,
automation and later collaboration share one mutation path. Node kinds describe their ports and
parameters as data, and the UI is generated from that description.

## Crates and layers

A crate may depend only on crates in strictly lower layers. `cargo xtask layers` checks this on
every CI run, and fails on an unregistered crate.

| Layer | Crate | Owns | Workspace deps today |
|---|---|---|---|
| L0 | `types` | ids, `Modality`, `PortType`, `Value`, hashes, money, the error base (pure data, serde) | — |
| L0 standalone | `media` | sniffing, decode/encode, thumbnails, metadata; image first, video/audio probes behind features | none allowed |
| L0 standalone | `plugin` | the plugin package and wire contract: IDs, canonical JSON, manifest, inventory digest, signatures, frames and messages, locks, preservation ([spec](plugin-wire.md)) | none allowed |
| L1 | `comfy` | ComfyUI client: servers, `/object_info`, workflow import, uploads, prompt queue, WebSocket progress | types |
| L1 | `plugin-store` | verifying `.ncraft-plugin` data packages and the local package store: install disabled, enable all-or-nothing, update, rollback, uninstall ([decision](decisions/2026-10-19-plugin-data-package-store.md)) | plugin, types |
| L1 | `graph` | the document: Project, Canvas, Node, Edge, Field, Comment, Tag, Element, Generation, TechniqueDef; `Changeset` apply/invert/validate | types, media |
| L2 | `nodes` | `NodeKind` trait, registry, built-in descriptors and parameter schemas | graph, comfy |
| L2 | `catalog` | provider and model descriptors, capability matching, pricing, auto-select rules | graph, types |
| L2 | `actions` | deterministic image/video/text actions (CPU reference, rayon) | graph, media |
| L3 | `exec` | planner (routers, batches, techniques → job DAG), run executor, scheduler, queue, cancellation, staleness, quotes, budgets | nodes, catalog, actions |
| L3 | `providers` | `Provider` trait and adapters, one Cargo feature each | catalog, comfy |
| L3 | `gpu` | wgpu actions, custom WGSL actions, 3D preview renderer | actions |
| L3 | `format` | `.ncraft` files, content-addressed asset store, atomic saves, migrations | graph |
| L4 | `engine` | `Session`, command registry, undo stack, event bus, settings, library; `build_info` | exec, providers, gpu, format |
| L5 | `agent` | in-canvas agent: LLM tool loop over engine commands | engine |
| L5 | `automation` | control channel (JSON), MCP server, HTTP serve mode | engine |
| L5 | `ui-egui` | egui shell: canvas widget, node cards, panels, editors, theme tokens | engine |
| dev only | `testkit` | graph fixtures, mock compatibility reexport, snapshot helpers | engine |
| dev only | `provider-testkit` | provider-only mock/type reexports for lower-layer tests | providers, types |
| exempt | `apps/nodecraft`, `apps/nodecraft-cli`, `xtask` | binaries and tooling | anything |

```mermaid
flowchart BT
  types --> graph
  media --> graph
  types --> comfy
  graph --> nodes & catalog & actions
  media --> actions
  comfy --> nodes & providers
  nodes --> exec
  catalog --> exec & providers
  actions --> exec & gpu
  graph --> format
  exec & providers & gpu & format --> engine
  engine --> agent & automation & ui[ui-egui]
  agent & automation & ui --> app[apps/nodecraft]
  engine & automation --> cli[apps/nodecraft-cli]
```

Further rules checked by `cargo xtask layers`:

- **UI toolkits only in the shell.** egui, eframe, winit, egui_kittest, egui_extras, rfd,
  `craft-ui` and `craft-quit` may be used by `ui-egui` and the apps only, not by `agent` or
  `automation` even though they share its layer.
- **`media` is standalone:** no workspace dependencies, so it can be published and reused.
- **Layered consumers use `testkit` and `provider-testkit` as dev-dependencies only.** Apps and
  tooling keep their existing exemption; standalone media permits neither. The provider facade's
  workspace dependencies are restricted to providers and types in every dependency kind,
  including rejection of facade self-edges before generic same-package exemptions. It exposes the opt-in `providers::mock` implementation and the
  same external provider types; lower crates use the facade only in tests. Provider unit tests
  use their local `crate::mock` and local Provider trait to avoid duplicate-crate trait identity.
  The higher testkit preserves `testkit::mock` by reexporting providers directly, without a
  forbidden normal dependency on another testkit crate. Neither testkit class is in the normal
  wasm sweep; `cargo xtask wasm` separately checks the facade and opt-in providers/testkit mock,
  and refuses the wasm gate if either the normal sweep or that extra invocation fails.

## Core data types

`nodecraft-types` exposes distinct UUID-backed IDs for document entities. IDs are supplied by
callers as bytes (platform code owns randomness) and serialize as UUID strings, including map
keys. `Modality` and `PortType` describe ports; `Value` is a tagged scalar, text, media or nested-list
value. Keep decoder recursion limits and file-size limits enabled when reading projects.

`AssetRef` carries a 32-byte BLAKE3 digest (hex on the wire), a validated concrete MIME value and
optional integer media metadata. Metadata is descriptive, never permission to allocate an
unbounded image or buffer. `Money` stores non-negative millionths of a named currency, with
checked arithmetic and explicit errors for overflow, underflow and mixed currencies. Micros
serialize as decimal strings so browser JSON clients preserve every bit. Currency validation
checks the three-uppercase-letter syntax, not membership in a currency registry.

`Error` carries a stable `ErrorKind` and a public-safe message. Higher-layer errors can retain
internal causes, but must scrub secrets before constructing this shared payload.

## Node kinds and parameter schemas

`nodes::Registry::builtins()` registers Text, Image, Video and Audio sources under stable IDs
`source.text`, `source.image`, `source.video` and `source.audio`. A `NodeDescriptor` declares its
category, named typed ports, parameter schema and quick-add shortcut. `instantiate` builds
detached graph data with defaults; commands insert it through changesets. Registry registration
checks descriptor validity, unique IDs and shortcut collisions. `compatible(inputs)` uses each
kind's `mode` predicate for the Turn-into picker; sources accept only an empty input set.

`NodeKind::plan` returns pure `JobSpec` data. A source job emits its existing text or asset on
`out`, without decoding, I/O, model selection or payment. Planning validates stored kind/ports
and schema values. An unconfigured media source can exist on the canvas but returns an error
when planned. The source graph fixture verifies each output and graph serialization/undo.
Actual graph-card rendering and generated controls are wired by the canvas issues.

The shared `ParamSchema` lives in `types` and is re-exported by `nodes`, allowing the model
catalog to reuse it without a same-layer dependency. Its controls describe text, choice, toggle,
slider, integer, colour, model, seed and asset fields. `resolve(values, ready)` validates supplied
values, fills defaults and returns visible parameter names; missing required visible fields are
errors when `ready` is true. Conditions reference earlier fields and respect hidden controllers.
All supplied values, including hidden fields, must remain type-valid and within aggregate limits.
Boolean, finite floating-point and exact signed/unsigned integer `Value` variants store scalar
parameters without string coercion; only full-width integer wire encodings use decimal strings.

### Instance schemas, configuration and migration

`descriptor()` is static palette metadata. UI, MCP and planners resolve a stored node with
`NodeKind::resolve_instance(node)` and validate its ports/parameters with `validate_instance`.
The default `instance` hook borrows the static descriptor; workflow/Technique kinds can return
owned descriptors derived from config. The checked wrapper validates graph payload limits before
invoking the hook and validates the resulting descriptor before callers clone controls/ports.
`instantiate_instance` follows the same path for creation. `instance_mode(node, inputs)` lets
dynamic kinds inspect configuration; `mode(inputs)` remains palette compatibility for static kinds.
Dynamic implementations must bound parsing of their own config (including JSON decoder limits)
and preserve the kind ID. Registry injection means these APIs work with custom kinds in tests.

`Node.config` is a defaulted map of typed Values, separate from editable model parameters. Use
stable owner namespaces and explicit version keys, for example `comfy.workflow.version` plus
`comfy.workflow.v1.mapping`. Kinds own the typed interpretation; graph only enforces map, key,
value/depth and aggregate byte limits. Structured provider data can be a bounded encoded Text
payload with decoder limits, or typed scalar/list entries. Preserve unrecognized config entries
when rewriting a node. Configuration edits use the existing `SetNode` changeset, so serialization,
undo and restore retain them without a second mutation path.

Stale parameters are rejected with a bounded key name and migration guidance; values are never
included in errors. A kind can implement `migrate` for its versioned config/schema. Callers use
`prepare_migration` to validate the original and proposed node, show its warnings and explicitly
apply the proposed node through `SetNode`. A failed migration leaves the saved document intact.
Never silently discard unknown overrides/config or apply migrations during pure planning.

Schema descriptions are optional tooltips for generated UI and MCP. Choice controls allow up to
16,384 options with 4,096-byte values/labels, within a 1 MiB aggregate schema text budget that
also includes names, labels, descriptions, defaults and conditions. Resolve validates each control
once per schema pass; it does not rescan control definitions for every supplied/default value.
Float and signed/unsigned integer controls support absent bounds. Integer steps are exact,
positive increments anchored at the explicit minimum, or zero when unbounded. Signed differences
use i128 so full-width ranges cannot overflow. Seeds accept an optional unsigned maximum.
Unbounded numeric fields need an editable numeric UI rather than an invented finite slider range;
UI code must preserve integer values without an f64 conversion. Float slider steps remain hints.

`ResolvedParams.values` retains all valid overrides for editing and persistence; adapters and
planners must use `visible_values()` or `into_visible_values()` so hidden fields are not sent.
Visibility currently supports equality against an earlier field. Choice values deliberately use
Text identifiers; numeric enums require explicit conversion by the owning adapter. Exact integer
MCP/CLI schemas must describe the tagged Value with a canonical decimal string, not JSON numbers.
Source card snapshots are added by canvas issue #57; this crate's golden graph tests cover pure
planning independently of rendering.

## Document edits

`nodecraft-graph` stores projects, canvases, nodes, typed ports and ordered edges. Stored maps
are read-only to callers. Detached nodes and annotation stubs are data; use `Change` or
`Changeset` to add or modify them. Indexed `AddGeneration`, `RemoveGeneration` and
`SetActiveGeneration` edits record only the affected history entry or selection, without
putting the entire node history in undo/autosave. Clear or switch an active entry before
removing it; insertion indices preserve exact history order through undo.

Changesets are ordered transactions. Payloads, referenced entity existence and count caps are
checked while staging; final relationships (DAG, input order, field/tag references) are checked
once after the entire batch. This permits swapping occupied input orders or replacing an
in-use field in one transaction. New edges require their endpoint nodes to exist first.
Snapshots carried by forward/inverse operations must themselves pass payload validation.
Any error leaves the live document untouched. Apply returns a serializable inverse, and
applying that inverse returns redo. Node removal captures incident edges in one restore
operation, preserving IDs and input order without expanding past the operation limit.

Validation checks references, map keys, entity identity, port modalities/cardinality, input
ordering and DAG structure. Cycle detection and value validation are iterative. Recursive
values are checked before cloning, with depth/item/text limits; positions must be finite and
within ±10 million world units. Project deserialization validates structural invariants too.
File/control readers still must bound input bytes and retain decoder recursion limits before
allocating document data. Optional collections/title/position default when absent; IDs and
node kinds remain required. The format layer must check its file/journal version before
parsing graph data: unknown future fields/variants cannot safely be edited and saved by an
older reader. Invalid snapshots return an error; recovery needs a separate raw-data path.

Current limits in `graph/src/validate.rs` are part of that format contract: 8 MiB serialized
JSON per node (including parameters/history), 64 MiB per changeset payload and its inverse,
and 256 MiB per project, in addition to entity/depth/item caps. Byte counting streams to a
bounded sink, without allocating an encoded copy. An edit whose undo exceeds its limit is
rejected before commit. Node additions reserve the larger restore operation's JSON size so
undo/redo cannot cross the cap merely by changing operation spelling. Format/container readers
may impose stricter input limits.

Transactions stage one document clone and validate the full result once; structural batches
no longer validate every growing prefix. The clone still scales with existing document data,
including history; eliminating that copy needs a separately reviewed undo-journal or overlay
implementation. Validation-count regression coverage protects against quadratic rescanning.

## wasm

Every layered crate, `ui-egui` included, must build for `wasm32-unknown-unknown`
(`cargo xtask wasm`). Filesystem access, threads and processes go through platform services that the
apps provide; network I/O uses `reqwest`, which works on both targets.

## The engine/UI seam

The shell (`ui-egui`) holds only view state: pan and zoom (`canvas::CanvasView`), open panels,
hover, selection highlights and uncommitted editor drafts. Cards read the engine graph and
node-kind descriptors directly. All accepted title, parameter and position edits dispatch commands;
rendering never writes automatic titles into the document. Each card has a transformed egui layer
so its schema widgets use world coordinates while pointer hit tests follow canvas pan/zoom.

`engine::Session` now owns a validated `Project`, its command and node-kind registries, undo/redo stacks and
revision notifications. Frontends call `enabled(id, &params)` for a disabled reason and
`dispatch(id, &params)` for a JSON result. `registry().specs()` exposes stable IDs, labels,
shortcuts, alternate bindings, machine-readable JSON Schema and typed parameter documentation. Initial commands live in `graph_cmds::specs()`:
`node.add`, `node.remove`, `node.move`, `edge.connect`, `edge.disconnect`, `edit.undo` and
`edit.redo`. `node_cmds::specs()` adds `node.title` and `node.param_set`; the latter validates the
proposed node against its registered schema, including hidden parameters. An explicit null removes
an override and restores a schema default; an omitted value is rejected. `edge_cmds::specs()` adds
the atomic `edge.rewire` and `canvas.delete`, and `create_cmds::specs()` adds `node.create`, which
instantiates a registered kind with its defaults (quick-add keys and the palette use it; `node.add`
still inserts exact detached graph data). IDs in parameters are caller-supplied. Handlers inspect an immutable
context and return effects; they cannot directly mutate the live document. Hosts may inject typed
immutable resources for selection/settings snapshots or explicit capabilities; missing resources
return Unsupported. Handlers may return `Effect::Service` with a capability id and bounded JSON request. The session
invokes an injected host callback under its guard, bounds the response and leaves document/history/
events alone. Missing services return Unsupported; the host owns external atomicity/idempotency,
and no retry is automatic. A callback error, invalid response or panic can occur after an external
side effect commits; hosts must reconcile it. The session guarantees only document/history/event
preservation, not rollback of external effects. This seam does not itself implement settings persistence or job enqueue.
`Effect::None` returns a bounded result without document, history, revision or event changes.
Consecutive effective moves coalesce across redundant pointer samples and non-document effects.
Selection deletion belongs to the canvas selection command; single-node removal has no Delete
binding. Frontends can map redo's Ctrl+Y alternate in addition to its primary shortcut.

Dispatch bounds JSON nesting, item count and text bytes before recursive deserialization.
Dispatch invokes every custom enabled hook. Builtin graph enabled checks preview the graph only
in `Phase::Preview`; dispatch defers that graph work to one `Changeset::stage` transaction.
The staged draft and inverse publish together. Borrowed canonical replay accounting reserves the
AddNode restore spelling and the complete changeset envelope, avoiding a second undo/redo staging
pass. Parameter decoding borrows JSON and reports allowlisted field paths and error categories,
never raw input values or arbitrary map keys. Native panic unwinding returns an
internal error while preserving document/history/events; abort-only targets still depend on
validation. History limits and explicit drag tokens are specified in the
[engine-session decision](decisions/2026-10-16-engine-command-sessions.md), with the later
[history-step limit update](decisions/2026-10-17-200-node-performance.md). Each
subscriber calls `events_since(cursor)` and advances its own cursor to the last notification;
an expired cursor requires reading the current document and revision again. These notifications
are an invalidation log, not a persistence or collaboration journal. Applied changeset events
remain dependent on a separately bounded journal/subscriber API: attaching full payloads to 256
notifications would otherwise multiply the 64 MiB graph transaction cap. Granular generation edits
use the same command effect path: undo retains only the affected entry or active selection,
and both the inverse and the canonical redo must fit the engine history budget before dispatch
publishes any document, history or event change.

## Shared Craft libraries

From [storytold/craft-libs](https://github.com/storytold/craft-libs), pinned to one immutable
`rev` in `[workspace.dependencies]` (bump in its own PR):

| Crate | Used for | Where |
|---|---|---|
| `craft-ui` | widget palette and theme helpers, icon catalogs, tabs, docking | `ui-egui` |
| `craft-quit` | platform-correct quit and close (macOS app menu, close requests) | `ui-egui`, `apps/nodecraft` |
| `craft-settings` | settings file, platform folders, portable mode, recovery | `engine::settings` |
| `craft-atomic` | crash-safe saves | `format` (M1) |
| `craft-pdf` | PDF rendering and text extraction | `media`/`nodes` (M8) |

## Build provenance

`nodecraft_engine::build_info` reports the workspace version plus the commit and build date baked
in from `NODECRAFT_BUILD_SHA` and `NODECRAFT_BUILD_DATE` (set by the packaging scripts and CI).
`nodecraft --version` and `nodecraft-cli --version` print it.

## Plugin planning

The [plugin RFC](plugins-rfc.md) proposes data-only packages first and a gated executable
feasibility phase. It is a delivery/contract plan, not a currently implemented plugin runtime.
Its phase A, the package and wire contract, is the standalone `plugin` crate
([specification and limits](plugin-wire.md)): pure decoding and checking, no I/O, no runtime.
Phase B's verifier and local store are `plugin-store`; data packages install disabled and
nothing in them executes.
Future integration must preserve the existing layers, engine mutation path and wasm support.

## CPU text actions

`actions::text` provides bounded split, replace, concat and named-template algorithms with typed
serde requests and stable action IDs. They do not mutate graph state; live adapters remain #88.
See [text action contracts and limits](text-actions.md).

### Canvas instance consumption

The graph-backed shell caches checked `NodeKind::resolve_instance` descriptors and validated
visibility by session revision. It borrows node/default values on idle frames and creates full
text drafts only for focused editors. Widget drafts and drag previews are view state; accepted
edits still dispatch the shared commands. `node.param_set` validates the resolved instance's
ports and proposed parameters before returning a SetParam changeset. Configuration remains
untouched. Builtin command schemas include all current scalar Value variants and Node.config;
null is the reset sentinel for a parameter, not a stored Value variant.

`canvas.stale` uses the same authoritative Session/Context node-kind registry as node edits and
rendering. Builtin sessions require no separate Registry resource; custom sessions inject kinds
once through `Session::with_registries`. The query remains read-only and its enabled check only
checks the canvas, leaving staleness computation to dispatch.

The response preserves `nodes` as the known stale set and adds `unknown_total` plus `unknown`
(null or `{node, cause, kind, reason}`). Only active results with recorded input hashes are
checked. Local invalid hashes do not themselves seed staleness; unknown is not proof of freshness.
The first diagnostic follows canvas NodeId order and identifies the actual rejected target,
source or router. It reuses the query's cached, bounded public-safe reason without another hook
call. More unknown results are counted rather than collected into an unbounded message list.
The existing 576-byte diagnostic reservation shares the hash query's sticky admission budget;
global refusal returns an error without a partial report. Enabled/Preview remain cheap. The
legacy `stale()` set API and quote callers keep their behavior; completed paid runs still retain
results with unknown hashes when hashing fails. Control/MCP expose the command's same JSON.

## Project persistence (#54)

`format::Snapshot` combines a validated graph, immutable content-addressed `AssetStore`, and a
checkpoint epoch. `encode`/`decode` are platform-independent stored-ZIP codecs; they never extract
paths or decode media. The version gate precedes graph deserialization. ZIP directory/local-header
validation runs before the ZIP library allocates its directory. Only `project.json` and canonical
`assets/<lowercase-blake3>` regular entries are accepted, with matching lengths, CRCs and hashes.
Unknown JSON fields are tolerated; optional graph collections default empty. IDs, kinds and format
version remain required. Unsupported versions produce an update error.

Initial limits are 64 MiB per archive, 4 MiB manifest, 1,024 assets, 16 MiB per asset and 48 MiB
aggregate media. Codec bytes are currently buffered; cloned asset stores share immutable buffers.
Saving includes only assets referenced by node parameters or generation history, including lists.
Large-project lazy disk storage is a later optimization: do not simply increase these memory caps.
Formats v1/v2/v3 exclude ZIP64, compression, encryption, directory entries, archive comments and extra
fields. These restrictions make the bounded interchange surface explicit.

Hosts construct `Session::with_storage(snapshot, storage)`. A `ProjectStorage` instance belongs to
one session. Native hosts provide `NativeStorage` with a private, session-unique recovery path;
an OS lock on a persistent recovery-side file prevents concurrent recovery writers and releases
on process exit. Project destination locks use a private per-user app-data directory (see below).
Native paths retain `PathBuf`; command parameters use UTF8 filesystem paths (native) or opaque
keys (web). Browser hosts implement `ProjectStorage`; `MemoryStorage` supports deterministic tests.
The desktop/CLI adapters will supply these services when they connect their document sessions.

`file.open`, `file.save` and `file.save_as` use the same registry/dispatcher as graph commands and
never open dialogs. Open fully validates the archive and creates its recovery checkpoint before
publishing a replacement session document; it resets undo/redo and preserves monotonic event
cursors. Discarding a dirty document requires explicit `discard_dirty: true`. A successful save
updates location and savepoint after atomic publication and breaks drag coalescing. Undo entries
carry content state IDs, so undo/redo to the saved state clears dirty even though revision increases.
`file_status()` plus `status_revision()` notify hosts of save/dirty/recovery changes independently
of `DocChanged`; saving does not manufacture document edits.

`ProjectStorage::prepare_open` returns `LoadedProject {bytes,access}` and retains the previous
active destination while staging a candidate. Decode/checkpoint failures call `cancel_open`;
only successful publication calls the infallible `commit_open`. Native load's eager convenience
API commits immediately; engine lifecycle code always uses the staged protocol. Writable hosts
without destination locks retain compatibility defaults; locking/read-only adapters must implement
and forward the complete protocol. `file.open` returns `{opened:true,read_only:bool}`.
`FileStatus.read_only` means the source cannot be overwritten: file.save is disabled/refused,
while local edits/recovery and Save As to a different writable destination remain supported.
Native canonical aliases cannot bypass that source restriction. A published Save As adopts its
new destination even if subsequent recovery checkpointing fails, retaining existing save semantics.
`save_with_access` reports the capability of a successfully published destination; its default
delegates to `save` and returns ReadWrite for nonlocking hosts. Locking wrappers must forward it.
Native admission uses a short per-canonical-directory OS gate (at most two seconds of waiting,
then Busy), shared by prepare-open and save. Existing destinations acquire their canonical project
guard before any write. New destinations canonicalize the actual published file and acquire that
guard before releasing admission. Postpublication identity/guard failure reports successful
publication with ReadOnly capability, never an unpublished-save error. Save results include
`{saved:true,recovery_ready:bool,read_only:bool}`; FileStatus and the saved location agree.
Directory gates are released after adoption, so unrelated files can remain open concurrently.
Existing symlinks and chains are supported only when the complete destination canonicalizes.
An unresolved/dangling leaf symlink is rejected before admission and reserved-path checks;
callers must choose the actual writable target. Only an ordinary absent leaf may fall back to
canonical parent plus filename. Missing parents, cycles and metadata/permission errors refuse
publication rather than guessing which directory an atomic writer would follow.
This assumes cooperating processes and stable filesystem names; external deletion/renaming,
noncooperating writers, hard-link aliases and different OS users are outside the lock protocol.
All format-native calls to the upstream PID-named writability probe share a process mutex;
the helper stays upstream. Independent users of that helper outside this crate are not serialized.

Recovery files contain a complete checkpoint plus append-only, BLAKE3-checked changeset frames.
Every edit, undo, redo and intermediate coalesced move is appended to the OS before publication.
Native recovery keeps its append handle open and a worker group-syncs pending bytes every 250 ms,
plus scheduler/filesystem latency. This bounds the ordinary power-loss window without syncing
every pointer sample. Save and explicit clean close flush synchronously. Worker failures latch
and surface on append/flush; an edit whose append fails does not publish.
Sync captures a cloned handle, file generation and appended sequence under the state mutex, then
releases it before syncing. A separate sync gate serializes worker/explicit syncs without blocking
appends. Success advances durable only through the captured sequence; concurrent appends remain
dirty. Each generation owns an atomic failure token checked before/after append, so a known failure
latches immediately without waiting for the state mutex. Checkpoint publication advances generation
and replaces that token; stale handle completion cannot clear new dirty data or poison a new file.
Explicit flush covers its invocation boundary, or a durable replacement checkpoint, and clean close
waits for that boundary before discard. Overflow is rejected before checkpoint/append publication.
Newly referenced asset bytes accompany the first frame that needs them. Frames are sequence- and
checkpoint-bound, at most 64 MiB; the journal is capped at 128 MiB and requires saving when full.
Replay validates graph and asset references after every frame. The checkpoint performs the full
asset walk; subsequent frames recheck only surviving nodes whose asset-bearing data changed
(AddCanvas, add/set/restore node, SetParam and AddGeneration). Unchanged references remain valid
because stored asset bytes are immutable. Config, nested Values and all generation outputs are
included. Removing a bad reference in a later frame cannot hide it. Moves need no asset walk;
graph apply still clones/validates the final project per frame, so overall replay remains
proportional to frame count times document size. Before accepting an edit, the
session preflights canonical forward/inverse replay with max sequence digits, the Record envelope
and every retained asset as potentially additional after a checkpoint. Full binary headers count
toward the64MiB frame limit, so accepted undo/redo cannot become oversized after saving an empty
or smaller document. Any physical outer framing/length/checksum damage yields the complete
validated prefix, even when later frames survive. `Recovered.damage` reports the first damaged
frame's absolute byte `offset` and typed `reason`; `truncated_tail` remains true for existing hosts.
The decoder stops there and never applies any later frame. Invalid checkpoints, checksum-valid
semantic/compatibility violations, missing/invalid assets, reordered and stale frames remain hard
errors. A valid outer checksum makes malformed inner record/asset envelopes semantic errors too.
An ambiguous append failure preserves live state and disables edits until a successful save
replaces the journal. `NativeStorage::recover` preserves the original bytes and latches a damaged
journal against append until a fresh checkpoint is published. Pure `journal::recover` cannot
mutate storage; other hosts must honor the same checkpoint-before-append contract and report
the reason/offset while retaining the evidence. Constructing a new
session from it writes a fresh checkpoint and presents it as untitled/dirty, requiring an explicit
save destination. Recovery selection/presentation belongs to the host; it never silently overwrites
a newer saved project.

`craft-atomic` writes same-directory staging files, syncs before publication and replaces the
destination. A storage save error means unpublished. Its directory sync after rename is best effort,
so success means published, not a guarantee against hardware failure. A recovery-checkpoint failure
after a published save returns `{saved: true, recovery_ready: false}`, records the saved location
and state, and disables edits until another save repairs recovery. Hosts must surface that status.


### Persistence integration boundaries and tracked storage work

Save encodes once: its archive slice is reused from the new checkpoint. Open decodes once and
reuses validated v3 archive bytes for checkpointing; v1 and v2 are explicitly converted to a v3 checkpoint
before adoption. After a save, asset GC preserves references
from both the live document and bounded undo/redo payloads; discarded redo assets can then be
reclaimed. Hosts must call `Session::close_recovery` on a clean close (or explicit dirty discard)
to flush and remove recovery; dropping a dirty session intentionally leaves recovery available.
Native project destinations use persistent lock inodes in the per-user NodeCraft app-data
`project-locks-v1` directory, named by a BLAKE3 hash of the canonical native path bytes. The directory
is private (0700 on Unix); symlink/unsafe lock entries are refused, and project destinations cannot
overwrite these private files. Lock files are never unlinked while participating in the protocol,
avoiding inode-split races and project-folder sidecars. A storage session holds only its active and
pending candidate guards, releasing the old guard after a successful Open/Save As and the candidate
on failure. Historical destinations do not impose a64-location limit. Actual contention is Busy.
A readable source in a read-only directory or with unavailable private lock infrastructure opens
explicitly read-only; writable saves always require a real destination guard and write capability.
The protocol coordinates cooperating processes for the same OS user and canonical path spelling.
Hard-link identity and writers under other OS users/other lock protocols are not coordinated; it
does not imply universal filesystem exclusion. Recovery-path protection remains separately enforced.
Format checks ZIP parser entry-count agreement. `types::Blake3` is canonical for project/journal
references; format's `canonical_hash`/`media_hash` bridge the standalone media cache key without
illegal orphan From implementations. Known MIME signatures and dimensions from available bounded image probes are
verified against bytes. Duration/sample-rate/channel/page metadata remains advisory until the
[media milestones](plan.md#7-roadmap) provide the corresponding bounded probes.

The bounded buffered M1 codec does **not** yet satisfy [plan 4.6's large-project requirement](plan.md#46-storage)
that assets stay on disk and load lazily. This remains tracked in [lazy asset storage #147](https://github.com/CypherNaught-0x/nodecraft/issues/147), dependent on #54:
a file-backed AssetSource, streaming archive/checkpoint paths, large video limits and ZIP64 need
a separate format/asset-store design before increasing memory caps. The current 16 MiB/48 MiB limits
are operational limits, not a claim of large-video support. Comments owns format v2 with frozen exact-v1 migration fixtures and an explicit NCRJ0001→NCRJ0002
checkpoint transition. Unknown versions reject before typed graph decoding. The persisted Tag/Field vocabulary is introduced by the v3 compatibility bridge (#193), with
NCRJ0003 recovery. Frozen v1/v2 paths still reject those reserved roles rather than dropping them.
Both older journal versions retain their original embedded archive hash while replaying, then
require an atomic v3 checkpoint before appending. The bridge adds no Tags/Fields commands or UI.
See [local comments, replay certification and format ownership](comments.md).

### Frozen-v1 bridge on the current graph

The v1 archive and `NCRJ0001` journal keep the exact08586e representation contract. A contextual
raw-JSON gate runs after container/version/checksum admission and before typed graph/changeset
decoding; writers apply the same gate to bounded manifests/records before publication. Node
configuration (including explicit empty/null keys in incoming v1), scalar Bool/Number/Integer/
Unsigned values in actual params/history, Note/discussion fields, generation input hashes and
reserved newer Tag/Field members require newer format support. Harmless extension objects
outside those roles retain the old serde behavior. Diagnostics are value-free `Unsupported`
errors. No fixture headers or version numbers are rewritten.

Current nodes with empty config and legacy Text/Media/List data remain saveable. Memory-only
sessions support the full current graph, including config/scalars and undo/redo. Storage-backed
sessions graph-stage prospective documents and preflight forward/inverse replay before append
or history publication. A validated v1 checkpoint plus complete frozen contextual forward-role
guards preserves compatibility inductively; unchanged representation needs no additional full
project traversal. Archive/Save/Open retain full validation and the 4-MiB manifest cap, without
imposing that smaller cap on incremental editing. Record preflight retains max-sequence,
canonical replay and binary/asset overhead reservations, then allocates an additional bounded
record buffer (at most16 MiB) and temporary parsed JSON tree for compatibility. This is an extra
allocation/pass, not zero-copy; the guard retains no accumulating metadata. Config media walkers
already cover live and retained-history references for the later v2 writer, although v1 refuses
nonempty config. Full config/scalar persistence arrives with the coordinated Comments-v2
migration; it is not silently added under v1. Large/lazy assets remain #147.

A local macOS debug-profile sample used a 262,445-byte canonical-shaped record containing a
256-KiB text parameter: 100 full replay preflights took 1.296 s. This includes canonical budget
traversal, the extra bounded serialized buffer and raw JSON parse/guard; it measures neither
fsync nor durability. The buffer length is measured, not a claim about total heap usage (the
parsed JSON tree is additional). The ignored `compatibility_preflight_cost` test reproduces the
sample without timing assertions.

Persistent preparation returns a private, non-Clone bundle pairing the graph-staged document
with an opaque admitted forward replay. It captures only referenced immutable asset bytes and
their canonical replay budgets. Append consumes this proof, derives additional assets from
forward references minus already-journaled hashes, and accepts no substitute document/store.
Surviving changed nodes receive incremental MIME/dimension/reference checks against the complete
source store; unchanged references retain the validated-root proof. Ordered valid-final batches
and every inverse/future canonical redo remain admitted through the same path. Arbitrary public
`journal::frame` callers still receive full replay admission.

The bundle carries a private session source identity and revision. Commit rejects a proof from
another session, earlier revision, asset import or save/asset-GC epoch before append. Only a
successful publication/import replaces identity; failed preparation/import/append preserves it.
Availability graph-stages document effects once through Session, including generic/custom commands;
handler preview callbacks only check editability/parameters. Read-only Preview early returns stay
cheap. Opt-in `validation-metrics` exists for deterministic downstream tests, with no product-build
counter cost. Graph cloning/application, structural validation and byte-budget serialization still
traverse the project per admitted edit; this is not a constant-time or linear-total edit claim.

### Runtime result recording and frozen-v1 storage (historical; current writers use Comments v2)

The current `exec::run::Run` returns caller-owned completed results; it does not mutate a
Session or automatically wire desktop execution. A host can turn a result into one changeset
with `record_changes` and dispatch it through the ordinary command admission boundary.
Memory-only sessions accept stamped results and retain undo/redo. Frozen-v1 storage refuses
`Generation.input_hash` with an actionable Unsupported error before journal append or document,
history and identity publication. The caller retains the completed result for explicit later
handling; the engine must neither discard its stamp nor silently widen v1 to save it.

### Local comments and notes

[Comments](comments.md) adds bounded flat threads and canvas/node anchors through graph changes,
engine document commands and a scoped UI view service. Node deletion detaches discussions; compact
restore metadata plus certified exact F/I/R staging preserves undo/redo budgets. `structure.note`
is a generated plain-text no-port node whose plan intentionally contains no jobs. No sharing,
authenticated author, provider call or comment search is implied. Draft capacities/Session identity,
explicit Save/Cancel, versioned migration and current format ownership are specified in that contract.


### Comments-v2 on the current persistence host

Comments advances current archive writers to version2 and recovery writers to NCRJ0002. Frozen
v1 reads retain the landed contextual guard, including Generation.input_hash and default/null
reserved-role refusal; original archive bytes still determine legacy frame identity. Opening or
recovering legacy data must establish a new atomic v2 checkpoint before appending v2 edits.
Current v2 admits config, scalar Values, stamped generations and local threads; future Tag/Field
roles still reject. The earlier frozen-v1 runtime/storage restrictions above describe the previous
writer and are superseded for current writes, not weakened for v1 reads.

Private prepared admission, immutable referenced bytes, source identity/revision, complete-source
changed-asset validation, ordered valid-final batches, actual canonical redo reservations, physical
prefix damage recovery, staged Open rollback, source access/locks and sync generations remain the
same persistence path. No second Comments save/append service exists. Save availability validates
the current bounded manifest; incremental edits keep their existing replay/graph limits rather than
imposing the archive's smaller manifest ceiling on every edit.

## Optional native video sidecar

`media/video` provides the standalone bounded native FFmpeg service described in
[media-video.md](media-video.md). It depends only on external Rust crates and configured installed
tools, not workspace layers or FFmpeg bindings. Preview/scrub/render are library APIs; timeline UI
is separate. Feature-on wasm exposes an Unsupported host setup path.

## Selected-node formatting

Selected-node formatting lives in graph's pure `layout` module and engine's `layout_cmds`.
The UI supplies a revision-stamped scoped geometry capability, including history-strip body
height at fixed reference zoom. Advisory Preview validates structure without solving or staging;
dispatch returns one MoveNode changeset through the private persistent admission boundary.
Grid and whole-DAG dependency modes move selected nodes only, with one undo step; see
[the decision](decisions/2026-10-18-selected-node-auto-format-70.md) and [UI design](ui-design.md).

### Colour tag mutations and filtering

The graph’s compact `SetNodeTags` operation stores only per-node tag IDs and their inverse. Its aggregate inner-assignment limit is independent of the outer changeset operation count, and generated inverses use the same validator. Engine `tag.put`, `node.tag_set` and `tag.remove` own document mutation through Session transactions. Scoped `canvas.tag_filter_*` commands delegate view state to the live host; headless persistent edits remain available. See [color-tags.md](color-tags.md) for strict command schemas, version3 persisted color metadata, filter/jump semantics and resource bounds.
