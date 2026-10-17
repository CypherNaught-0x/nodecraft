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
| L1 | `comfy` | ComfyUI client: servers, `/object_info`, workflow import, uploads, prompt queue, WebSocket progress | types |
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
| dev only | `testkit` | fixtures, mock provider, snapshot helpers | engine |
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
- **`testkit` is a dev-dependency only.**

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
validation. History limits and explicit drag tokens are specified in `decisions.md`. Each
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
