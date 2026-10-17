# RFC: bounded plugin packages and execution

Status: planning proposal for [#154](https://github.com/CypherNaught-0x/nodecraft/issues/154),
2026-10-17. This RFC introduces no runtime, install command, schema implementation or marketplace.
The decisions accepted here constrain future implementation; the numeric limits are proposed
initial ceilings, not measurements or claims about the current app.

## 1. Scope and existing boundaries

Support independently distributed catalog data and reusable descriptors first. Later support
third-party Rust-authored node kinds, deterministic CPU actions and provider adapters through a
bounded wire contract. Commands may expose those functions, and declarative UI contributions may
add parameter groups, palette entries and inspector sections. No arbitrary egui callbacks, native
windows, shaders embedded as UI code, HTML or webview panels. Generated controls use host theme
tokens, accessibility labels and the same headless validation.

A Technique packages a graph and its exposed parameters; reuse it instead of installing code to
share a workflow. Its required plugin locks travel with the graph. Custom WGSL actions (#90)
remain the host's validated GPU-action path, not a plugin escape hatch. ComfyUI custom nodes (#9)
execute on the separately managed ComfyUI server; a package can describe a template and its
requirements, but installation never installs Python or server custom nodes. MCP (#116) exposes
host commands/tools to outside agents; it is not an extension loader, privilege grant or a reason
to create another mutation path. A plugin command may be described through MCP only after host
registration, authorization and equivalent schema validation. Custom text definitions/execution remain [#165](https://github.com/CypherNaught-0x/nodecraft/issues/165);
its bounded interpreter/Wasm work should supply shared execution machinery, not be duplicated by
a plugin runtime project. Agent skills [#168](https://github.com/CypherNaught-0x/nodecraft/issues/168)
are untrusted procedural instruction data, not executable plugins or Techniques. A data package
may reference a versioned skill, but import/read never activates it or grants execution rights.
Reuse existing deterministic action
algorithms rather than implementing another text/image library inside the loader.

Current main at `52c76801` has the crate skeleton, graph types and scheduler, not this plugin API.
The source-node/engine work and catalog PR [#164](https://github.com/CypherNaught-0x/nodecraft/pull/164)
are separate integration work. In particular, PR #164 proposes `ParamSchema` reuse, catalog
validation/quotes and name-based model migration; none is assumed merged here. The stronger
[#162 contract](https://github.com/CypherNaught-0x/nodecraft/issues/162) owns versioned parameters,
ports, precedence and migration semantics. Plugin descriptors must consume that contract rather
than define a competing schema. Names alone must never prove semantic compatibility. Pricing
entries require documented provider facts or recorded fixtures, not guessed cloud prices.

MVP excludes arbitrary executable plugins, marketplace discovery, automatic downloads on project
open, dependency build scripts, native dynamic libraries, plugin-to-plugin calls, arbitrary
process launching, raw filesystem/network handles, direct secret access and paid execution.
A later executable MVP adds pure actions only after the feasibility gates below pass. Provider
adapters and optional privileged capabilities come after that; unsupported capabilities remain
visible and disabled on every platform.

## 2. Approach comparison and recommendation

| Approach | Strength | Cost / trust boundary | Placement |
|---|---|---|---|
| Data-only package | Portable, reviewable; host validates all descriptors and referenced built-ins | Cannot add algorithms; bounded declarative expressions only, no scripts | First distributable MVP |
| Statically linked Rust extension | Normal Rust traits, same compiler and tests; existing layer checks | Trusted product code; an abort or resource abuse can affect app; update requires rebuild | First-party development/reference adapter, not third-party isolation |
| Sandboxed Rust-to-Wasm module | Explicit imports and guest memory boundary; one versioned wire surface | Host calls still need validation; cancellation, metering and browser embedding need evidence | Preferred candidate for later pure executable plugins, conditional on spike |
| Out-of-process Rust worker | Native crash boundary; can reuse wire protocol | A process is not a security sandbox; OS isolation and termination differ; no browser subprocess parity | Native fallback experiment, disabled until separately reviewed |
| Native dynamic ABI | Potential direct-call performance | Rust ABI is not stable; unsafe loader/FFI and allocator/unwind lifetimes conflict with project invariants | Rejected for MVP; no ABI chosen |

The Rust Reference explicitly gives the Rust ABI no stability guarantees
([ABI reference](https://doc.rust-lang.org/reference/items/external-blocks.html#abi)).
This is not evidence that a C ABI alone would provide sandboxing or satisfy the no-unsafe rule.
Wasm's core specification makes environment access an embedder responsibility through imports
([security considerations](https://webassembly.github.io/spec/core/intro/introduction.html#security-considerations)).
Wasmtime documents a safe embedding API and memory isolation, while also discussing sandbox bugs
and defenses; that makes it a candidate to evaluate, not a selected dependency or proof of all
host safety ([Wasmtime security](https://docs.wasmtime.dev/security.html)). No engine source was
copied or vendored for this RFC.

Recommendation: ship data packages, freeze a small versioned message contract through conformance
fixtures, then decide executable support using the spike in §11. Do not bind the public contract
to Rust layouts, a specific engine, WASI version, component model or guest allocator. Candidate
embedders must offer a safe host API; no local unsafe, raw-pointer ABI or unchecked guest offsets.
Reusable helpers belong in audited craft-libs where applicable, with a separate pin change.

## 3. Layering and platform matrix

Proposed ownership (crate names are illustrative and require a future `xtask layers` change):
L0 `types` owns pure wire identities/errors; descriptor semantics belong at the lowest layer
settled by #162. L2 node/action/catalog adapters consume validated descriptors and pure plans;
they do not depend on `engine`. L3 execution/provider adapters turn validated plans into jobs.
L4 engine owns lifecycle, grants, command registration, quotes, undo and effect commit. Platform
services supplied by apps own installation storage, clocks, keychain, network and worker control.
L5 UI renders schemas, consent and diagnostics only. The runner consumes immutable request data
and a capability broker; it cannot hold a Session or mutable document reference. Avoid an
engine→plugin→engine cycle by injecting trait implementations from the app.

| Facility | Native proposal | Browser proposal |
|---|---|---|
| Data parsing/schema validation | Same bounded Rust code | Same bounded Rust code |
| Package storage | Platform service, atomic local store | Platform service, bounded persistent storage if available; otherwise session-only and explicit warning |
| Guest execution | Candidate embedded sandbox behind safe Rust API | Candidate Rust interpreter or supported browser host binding; not assumed to exist |
| Files/processes | Brokered assets only initially; processes denied | Unsupported; no subprocess emulation or silent cloud delegation |
| Network/secrets | Later host broker, scoped grants | Later host broker subject to browser restrictions; unsupported requests fail before submission |
| GPU | Later host action API only | Same validated action API only where supported |

Rust's `wasm32-unknown-unknown` target does not supply usable OS filesystem or thread-spawn
services ([target documentation](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html)).
Compiling a library for wasm therefore does not establish browser runtime parity. No JS/TS plugin
runtime or webview is introduced; any platform glue must follow the existing Rust app policy.
If a safe Rust-compatible browser runner cannot meet the gates, ship data-only support there and
show `UnsupportedCapability` for executable kinds without discarding their data. Browser support
must never be simulated by running a local process or uploading a project without consent.

## 4. Package identity, registration and contract

Package IDs use reverse-DNS-style lowercase ASCII labels (`io.example.textpack`); labels match
`[a-z][a-z0-9_]{0,23}`, 3–6 labels, at most 80 bytes total. This is a logical namespace, not a DNS
hostname; hyphens are rejected rather than silently replaced. Owning a
DNS-looking string is not authenticated ownership. Publisher key identity plus package digest
pins trust; a local package is explicitly unverified until trusted. Local entry IDs are lowercase
ASCII dotted segments matching `[a-z][a-z0-9_]{0,14}`, at most 2 segments. Registered IDs are
`plugin.<package-id>.<local-id>`, at most 128 bytes (119 under these sublimits); host-owned prefixes
are reserved. This stricter grammar fits the reviewed engine's lowercase/digit/underscore dotted
128-byte command limit and graph's 256-byte kind limit without changing saved IDs. Phase A freezes
shared boundary fixtures; phase D must implement bounded owned registration lifetimes rather than
assuming today's static command registration API can load plugins or leaking strings to make IDs
static. Display
labels/translations never select behavior. Collision anywhere in a batch rejects the entire
registration; do not let install order shadow another package or built-in.

A manifest lists all registrations, descriptor digests, required capabilities and optional ones.
Discovery scans only the explicit package store/index, not projects, PATH, arbitrary directories
or media archives. Registration is staged, bounded and atomic: validate every item and dependency
before publishing any entry. Module execution cannot register additional kinds at run time.
Schema labels/descriptions are plain text with bounded length; no executable visibility or
validation expressions. #162 must provide bounded serializable conditions, exact integer/seed
wire values, enum IDs separate from labels, required/default/null policies, input role ordering
and cross-field checks. Hidden values remain preserved according to that contract and are still
validated before planning. Plugin UI and headless invocations use identical resolved settings.

Proposed original manifest, for the later executable phase, not a working package today:

```json
{
  "manifest_version": 1,
  "package_id": "io.example.textpack",
  "version": "1.2.0",
  "host_protocol": {"major": 1, "min_minor": 0, "max_minor": 0},
  "descriptor_contract": {"major": 1, "minor": 0},
  "publisher_key_id": "example-test-key",
  "license": "MIT OR Apache-2.0",
  "runtime": {"kind": "rust_wasm", "module": "guest.wasm"},
  "dependencies": [],
  "capabilities": {"required": ["asset.read", "asset.write"], "optional": []},
  "registrations": [{
    "id": "normalize", "kind": "action",
    "descriptor": "descriptors/normalize.json"
  }]
}
```

`descriptors/normalize.json` is illustrative #162-shaped data, not its final schema:

```json
{
  "descriptor_version": 1,
  "id": "plugin.io.example.textpack.normalize",
  "parameters": [{"id": "plugin.io.example.textpack.form", "type": "choice",
    "values": [{"id": "nfc", "label": "NFC"}], "default": "nfc"}],
  "inputs": [{"role": "text", "type": "text", "min": 1, "max": 1}],
  "outputs": [{"role": "text", "type": "text", "min": 1, "max": 1}],
  "deterministic": true,
  "affects": ["staleness", "cache", "output_shape"]
}
```

Data-only packages instead declare `runtime.kind=data` and may supply validated catalog data,
Technique references or descriptors bound to an explicit allowlisted host implementation ID.
A data package cannot turn an arbitrary action ID into executable behavior; unknown bindings
remain disabled. Provider/model catalogs must reuse #77's validation/quote path once merged,
with #162's eventual compatibility rules replacing simplistic name-only assumptions.

## 5. Wire, lifecycle, jobs and mutation authority

All examples describe protocol major 1. Byte framing is a 4-byte unsigned big-endian payload
length followed by UTF-8 JSON; reject the length before allocation. Each frame is at most 1 MiB,
with decoder depth 32, unique object keys, bounded strings/arrays and unknown fields rejected.
Integers that need full u64 precision, including revisions/units/micros, use canonical decimal
strings; floating values must be finite and follow #162. No Rust pointers, paths, URLs with
credentials, secret values or serialized engine objects cross the boundary. Asset handles are
opaque session-scoped tokens bound to a job and grant epoch, not filesystem names.

States: discovered → validated → installed-disabled → granted → enabled → draining → disabled.
An invalid package is quarantined outside the active index. `initialize` negotiates protocol and
returns declared descriptors only; it cannot start work. There is no arbitrary guest start hook
with ambient services. Each invocation has request/job IDs and bounded `accept`, `progress`,
`result`, `error` and `cancel_ack` messages. Progress is monotonic 0–1000 per job, rate-limited;
unknown, duplicate or out-of-order job messages are rejected or recorded as bounded diagnostics.
Exactly one terminal host state wins. Late output after cancellation is discarded, and staged
assets are released. Guest errors carry stable codes (`InvalidInput`, `UnsupportedCapability`,
`ResourceLimit`, `Cancelled`, `Conflict`, `Internal`) and field IDs; the host maps these onto its
safe error taxonomy, strips input values/control sequences and caps diagnostic retention.

Example command request after the user selects a text input (symbolic handles, not real assets):

```json
{"protocol":1,"type":"invoke","request_id":"req-7","job_id":"job-7",
 "command_id":"plugin.io.example.textpack.normalize",
 "package_digest":"blake3:0000000000000000000000000000000000000000000000000000000000000000",
 "expected_revision":"42","grant_epoch":"3",
 "params":{"plugin.io.example.textpack.form":"nfc"},
 "inputs":[{"role":"text","handle":"asset-job7-input0","bytes":"12"}],
 "output_budget":{"bytes":"1048576","items":1},
 "idempotency_key":"nodecraft:example-run:job-7"}
```

Zero digest here and below is a syntactically shaped fixture placeholder, never a trusted package.
The host validates invocation/schema, revision, grants and resource reservation before enqueue.
This normalization job quotes zero and cannot acquire network/secret/process access. Guest reads
only its granted input stream, writes output into bounded staging, then proposes:

```json
{"protocol":1,"type":"result","job_id":"job-7","grant_epoch":"3",
 "outputs":[{"role":"text","handle":"asset-job7-output0","bytes":"12"}],
 "proposed_effect":{"kind":"attach_output","target_node":"host-bound-node"}}
```

The target is an opaque host-bound token, not a guest-chosen document path. Host validates actual
stream bytes/modality, output schema, current epoch and expected revision, then constructs the
engine command/changeset. Output count/size in the message is an assertion to check, not allocation
authority. A plugin never sends trusted graph changesets or executes arbitrary host commands.
The host validates any command intents against a small declaration-specific allowlist; edits
apply atomically and create one undo step per logical action. UI previews and progress are view
state. Validation failure/stale revision leaves document/history unchanged and returns Conflict;
a user may explicitly retry against a new snapshot. Future paid jobs attach immutable generation
results using the engine's run identity rules, rather than assuming an old document revision is
still current. Redo restores recorded effects, not external execution or another paid call.

Cancellation closes brokers, advances the grant/job epoch, stops accepting results and requests
cooperative stop; after the grace period, terminate/reset the sandbox. A host cancellation is
terminal even if `cancel_ack` never arrives. No plugin loop, trap, abort, worker exit, invalid
message or exhausted queue may crash the app or wedge the scheduler. Plain statically linked
code cannot promise this isolation and is not an untrusted executable package backend.

### Worked lifecycle: one normalization package

1. User explicitly imports the package. The host streams it into quarantine, checks paths, sizes,
   inventory digest, licenses, protocol and descriptor compatibility without running its module.
2. It installs disabled; the user reviews publisher status and grants only `asset.read/write` for
   this digest/workspace. Host stages registration atomically and enables the action. A headless
   caller must supply a pre-existing grant; missing consent is an error, not a dialog.
3. UI or CLI dispatches the registered engine command with the same schema. Host resolves the
   NFC parameter, binds one text input, snapshots revision 42, reserves resource budgets and
   supplies the zero-cost quote. Unknown/missing inputs fail before guest entry.
4. A job gets the request above. The guest reads bounded chunks through its input token and
   writes staged output chunks; chunk frames share the 1 MiB frame cap and queue backpressure.
   Progress does not mutate the document.
5. On success, the host verifies actual output and the current revision/grant epoch, then commits
   the single host-owned attachment changeset. Undo/redo restores data without rerunning the guest.
   If the user cancels instead, the host invalidates job handles/epoch, rejects later output,
   releases staging and resets the runner after the grace period; no attachment is committed.
6. Saving records the package ID/version/digest, descriptor snapshot and bounded instance data,
   plus referenced output assets, but no grants or paths. Reopening with the package unavailable
   shows the retained generic card and output; it cannot run or silently install anything.
7. Installing that exact digest and granting rights again restores validated execution. Updating
   to another digest requires explicit compatibility/migration review and a fresh quote when paid
   work is involved; denial leaves the original saved node intact.

## 6. Capabilities, paid work and privacy

Default deny. Installation may inspect metadata but never enables execution or grants rights.
The host consent UI lists required/optional rights, publisher verification and platform support.
Required rights denied means installed-disabled; optional rights denied gives an explicit
capability-unavailable response. CLI/MCP callers pass an existing grant ID; they never trigger
interactive dialogs or silently accept new permissions. Grants are per package digest, workspace
scope and capability; updates cannot inherit broadened rights. Revocation increments an epoch,
blocks new work immediately, cancels queued/running work and prevents subsequent effect commit.
A request already delivered to an external provider may not be undoable; surface that status.

| Capability | Grant / broker policy | First executable phase |
|---|---|---|
| Asset read/write | Explicit immutable input handles; bounded staged outputs; no store enumeration | Allowed |
| Network | Later HTTPS origin + method/path-template allowlist; validate redirects/DNS destinations, disallow local/private targets unless separately granted; byte/time/concurrency caps | Denied |
| Filesystem | Explicit user-selected file/dir tokens, canonicalized traversal-safe broker, no ambient paths; symlink/reparse-point checks | Denied |
| Processes | No shell; separately reviewed executable identity + bounded args/env/stdio, native-only | Denied |
| GPU | Host-validated #90 action/texture handles and device budgets; no raw device access | Denied |
| Secrets | Host binds opaque credential-profile reference to an approved provider operation; raw keys stay in OS keychain and never enter guest memory | Denied |

A provider plugin later returns a declarative request plan. A host-owned provider broker selects
credentials, constructs authenticated transport, scrubs logs and forwards only bounded approved
response fields. This is not general secret injection into arbitrary plugin-generated HTTP.
Credentials cannot be combined with a guest-selected origin, headers, redirects or telemetry.
No automatic analytics, registry contact, update check or crash upload. Public-safe diagnostics
include package digest/code/timings, never prompts, assets, credentials or private paths.

Paid planning must resolve exact package/descriptor/model/version, effective parameters, input
identities, fixed seeds and job count before quoting. Host authoritative quote uses exact Money,
upper bounds for unknown usage, expiry and approved currency/budget; an unbounded or unavailable
quote disables submission. Reserve budget before calling a broker and use the same host-generated
idempotency key on retry. A plugin's claimed zero price cannot override a host-known paid operation.
Record request identity before dispatch; unknown submission outcome requires reconcile by key,
not blind resubmission. Unsupported provider idempotency requires no automatic retry after an
ambiguous submit, with a clear possible-charge state. Cancellation does not promise a refund.
No live/paid services in conformance tests.

## 7. Limits and scheduling

All limits are hard host ceilings checked before allocations/copies/compilation; a manifest can
request less, never raise them. Product budgets below are initial proposals requiring measured
review in §10/11, not license to postpone deterministic bounds.

| Resource | Initial ceiling / policy |
|---|---|
| Download/package | 32 MiB compressed, 128 MiB total expanded, 1,024 entries; streaming caps independent of declared sizes; no nested extraction |
| Metadata | 256 KiB manifest, 256 registrations, 1 MiB aggregate descriptors, depth 32, 16 dependencies; #162 may impose stricter per-schema caps |
| Identity/text | IDs as §4; labels 256 bytes, descriptions 4 KiB each within aggregate; diagnostics 4 KiB/frame, 64 KiB retained per package |
| Wire/queues | 1 MiB/frame; 32 pending messages and 4 MiB queued bytes/package; 64 MiB aggregate queue/descriptor budget/session |
| Guest | 64 MiB linear memory, 1 instance/package initially; 16 installed enabled executable packages/session, 2 active jobs/package and 8/session |
| Job input/output | 16 MiB input streams, 16 MiB output streams, 64 output items/job; aggregate staging 128 MiB/session; stop streams before cap, never truncate |
| Time/work | 2 s initialize, 100 ms planning, 10 s pure CPU job; 250 ms cooperative cancellation grace; deterministic instruction/fuel ceiling selected by spike; fail closed if not enforceable |
| Progress/logs | 10 progress events/s/job, 1 KiB progress payload; bounded ring logs; host coalesces before UI delivery |

Total guest memory admission is also capped at 256 MiB/session, irrespective of installed count;
queued jobs do not instantiate guests. Media-heavy actions need separate reviewed budgets rather
than automatically raising these ceilings. Stream sizes, recursion, integer arithmetic, handle
counts and graph traversal are hostile inputs. Errors return no partial document mutation.
Host broker deadlines, output quotas and admission control apply even when guest fuel is exhausted.
No guest work or synchronous IPC occurs on the UI frame path. Cached descriptors share immutable
storage by digest under the aggregate bound, including culled/invisible nodes; no per-node schema
copies. Releasing a grant/job frees all reservations even on trap or disconnect.

## 8. Packaging, trust and operations

A `.ncraft-plugin` archive contains `manifest.json`, descriptors/data or a module, `LICENSE*`,
`NOTICE` where required, and a complete file inventory. Inventory paths are portable relative
UTF-8 ASCII paths, at most 128 bytes and 8 segments; reject absolute paths, `..`, `.`, empty
segments, drive letters, backslashes, control characters, case-fold collisions, duplicate entries,
links (including symlinks/hardlinks/reparse targets), devices and unlisted files. Extract into a
private staging directory using safe platform APIs, never follow existing links, and check streamed
expanded bytes/counts before writing. No build hooks, installer executables or post-install code.

Canonical identity: parse manifest with duplicate-key/unknown-field rejection, forbid floats in
package metadata, sort object keys by UTF-8 bytes, emit minimal UTF-8 JSON without insignificant
whitespace, preserve string codepoints without Unicode normalization. Hash each listed regular
file with BLAKE3, excluding `inventory.json` and detached `signature.json`. Inventory stores sorted
path, exact byte length and lowercase 64-hex digest, including canonical manifest bytes (the
archive manifest must already equal those canonical bytes). Compute package digest as BLAKE3 of
`nodecraft-plugin-v1\0` followed by canonical inventory JSON. Inventory contains no package digest;
the detached signature signs that domain plus the 32-byte package digest, preventing a recursive
self-hash. The wire prefix `blake3:` is not part of the digest. Canonicalization and signature
algorithm/verification details must be frozen by phase A fixtures before shipping an installer;
no signature algorithm or trust registry is selected by this RFC.

A digest establishes integrity, not publisher trust, safety or a legal license. A signature proves
only possession of a recognized key; key rotation/revocation requires a separately reviewed local
trust update. MVP installation is explicit local import, with provenance status and a user-visible
content digest; no central registry/marketplace prerequisite. Dependencies are data packages only,
acyclic, maximum depth 8; exact version + digest locks, no semver resolver or network fetching.
Missing/conflicting locks prevent enabling the dependent package. One active version/package per
workspace initially; incompatible projects use separate environments, never install-order wins.

Install validates/extracts/scans before atomic activation of an index; full disk, corrupt archives
or invalid license metadata leave previous state untouched. Update stages side by side, checks
compatibility and proposed migration, pauses new jobs, drains/cancels existing jobs, then swaps
index atomically. Failure restores the previous package/index only. Re-evaluate current trust and grants;
never roll back revocation epochs or resurrect a grant revoked during draining. Leave the package
disabled when no currently valid grant remains. Broader permissions require new consent. Old package objects stay while referenced by projects/history/rollback, under explicit
storage quota and user-managed cleanup. Uninstall disables first and removes only unreferenced
content; projects and generated assets remain. Disable/uninstall never edits nodes or runs a
migration. Local development imports the same reproducible package via a proposed dry-run/lint
CLI, explicit reload and mock services, not an unrestricted watched source folder. No code runs
during lint. CI exercises the same verifier used for installation.

Rust authors follow repository no-unsafe/no-panic/layering rules and permissive licensing policy.
Packages declare SPDX license expressions and bundle required license/notice files for all
redistributed assets/code; host validation is not a legal compatibility guarantee. Original work
and any permitted adapted asset need provenance/attribution; do not copy GPL/LGPL/AGPL code or
study its source for this implementation. A plugin cannot weaken host policy with its own license.

## 9. Compatibility, save/open and migration

Negotiate protocol major equality plus intersection of minor ranges and explicit optional
features. Descriptor contract versions negotiate independently; unknown required features reject
enable, unknown optional features are ignored only when declared optional with tested fallback.
Do not deserialize unknown enum variants as a default. Package semver is author intent, not
compatibility proof; exact digests identify execution. Changing behavior under the same version
but a new digest requires explicit trust/update and invalidates dependent cache/quotes.

Proposed project format extension (requires #54 version gate): required package ID/version/digest,
protocol/descriptor versions, used registration IDs, bounded descriptor snapshots and opaque
instance config version/blob. Record effective model/params/input order/fixed seeds and package
identity with generations. Never save consent grants, local install paths, credentials or worker
state. Unknown plugin data is bounded opaque JSON with unique keys, retained byte-for-byte as an
opaque payload alongside validated generic node layout/edges/assets; the format must define its
canonical envelope separately. Unsupported future project versions remain read-only or fail
clearly under existing format rules, not rewritten by this RFC.

On missing/disabled/revoked/incompatible plugin: render a generic labeled placeholder with saved
port snapshot and existing output references; disable execution and semantic edits. Allow generic
move/select/delete through normal changesets while retaining the opaque payload; deleting a node
is explicit document editing, unlike uninstall. Re-saving a supported envelope preserves unknown
payload and requirements. Reinstalling a matching trusted digest restores availability after
validation; never download/enable automatically. A newer package may offer a bounded pure
migration from a specific old descriptor/config digest. Host runs it on a copy, validates with
#162, shows field-level dropped/reset/incompatible notices without values, and commits one undoable
changeset only after approval. Failed migration leaves the original and history unchanged;
rollback restores the old package lock and data. Never infer equivalent units, enum meanings or
ports merely from matching names. Paid work is blocked until migration and quote are resolved.

## 10. Conformance, failure and performance gates

A Rust conformance runner owns hand-written original public-safe fixtures for manifest/wire/
project round trips; exact canonical digest/signature vectors; schema parity across UI/headless;
registration collision atomicity; dependency cycles; unknown/missing data preservation; version
negotiation and migration undo/redo. Hostile tests cover duplicate keys, deep recursion, numeric
overflow, huge declared/streamed sizes, compression bombs, traversal/link/case collisions, malformed
guests/handles, forged job IDs/epochs, capability escalation, redirect credential exfiltration,
quota exhaustion and full disk. Fuzz every decoder; property tests assert bounded output and no
panic/partial mutation. No secrets or real providers in fixtures.

Runtime gates test infinite loops, memory growth, traps, worker disconnect, stalled host calls,
out-of-order/duplicate/late results, revocation between validation and commit, cancellation timeout,
ambiguous paid submission/idempotent retry, failed update concurrent with grant revocation,
crash recovery and reservation release. Native tests
cover Linux/Windows/macOS; browser runtime tests cover each supported browser/backend matrix in
addition to wasm compilation. Unsupported features have explicit tests and disabled UI state.
First-party linked adapters and third-party guest adapters must produce equivalent pure-action
results; no demand for provider-level bitwise reproducibility.

Use [#151](https://github.com/CypherNaught-0x/nodecraft/issues/151)'s bounded, content-free metric
report and [#152](https://github.com/CypherNaught-0x/nodecraft/issues/152)'s release comparison
harness; do not create competing instrumentation. Proposed admission targets: warm no-op request
p95 overhead ≤2 ms (1 KiB request/result); 1 MiB round-trip ≤20 ms p95; 100 descriptors validate
within 100 ms p95; cancel acknowledgment/forced termination ≤500 ms p95; idle plugin system adds
≤0.5 ms p95 CPU frame preparation for the shared 200-/1,000-node canvas workloads. Cold start is
reported separately with ≤2 s target and peak memory ≤64 MiB/instance plus measured host overhead.
These thresholds are proposals to accept/revise with measured evidence before release, not results.
Record warmup/sample counts, native/browser environment, revision/profile and p50/p95/p99, memory
and bytes copied where supported. Missing memory metrics are unsupported, not zero. No-op setup is
outside timing; instrumentation enabled/disabled overhead is measured. CPU measurements never
prove GPU/display 60 fps; #65 retains that exit criterion. Noisy shared CI is informational until
#152 establishes compatible runners/baselines; deterministic work/byte limits gate from day one.

## 11. Delivery phases and bounded uncertainties

The reviewed independently claimable follow-ups below own implementation; claim them through
the normal issue workflow. Their `future` labels indicate dependencies, not work already shipped.
Existing #162, #77/#164, #54, #90, #116, #151/#152, #165/#168 and provider/action issues retain their ownership.
The RFC does not claim or duplicate their implementation.

| Phase | Suggested milestone / dependencies | Exit / responsible role |
|---|---|---|
| [A: wire/package contract fixtures](https://github.com/CypherNaught-0x/nodecraft/issues/171) | M1 shared foundations; #162 descriptor semantics and #54 preservation coordination | Contract maintainer freezes IDs/canonical digest/version/missing-data fixtures, no runtime dependency |
| [B: data-only package verifier/store](https://github.com/CypherNaught-0x/nodecraft/issues/172) | M4 reusable actions/catalog; A + merged #77 data contract + #54 | Storage maintainer ships bounded atomic install/disable/update/uninstall and inert-data adapters, native/wasm errors explicit |
| C: safe executable feasibility (existing #165, coordinated deliverable) | M4, after A; consumes #151/#152, independent of B implementation | Runtime maintainer + security reviewer deliver reproducible native/browser results and safe-API dependency audit, then explicit go/no-go |
| [D: plugin executable adapter/lifecycle](https://github.com/CypherNaught-0x/nodecraft/issues/173) | M4 after B+C pass, shared action nodes #88 and #162; reuses #165 execution | Runtime/engine maintainers integrate package identity/grants with #165 runner and prove cancellation/migration conformance; no privileged services |
| [E: provider/capability broker](https://github.com/CypherNaught-0x/nodecraft/issues/174) | M5 extension of M3 provider contracts, after D and budget/idempotency foundation | Provider/security maintainers prove consent/revocation/paid ambiguity tests; no raw keys in guest |
| [F: authoring/conformance workflow](https://github.com/CypherNaught-0x/nodecraft/issues/175) | M5 reusable packages, after B; executable examples only after D | Tooling maintainer ships lint/package/mock fixtures/docs and CI matrix, no marketplace |

Phase C is a proposed bounded spike within existing #165, coordinated with its owner; do not open
a competing custom-text runtime issue. Plugin-specific adapter gaps belong to phase D.
The shared spike is: at most two candidate safe-API engines/embeddings, one original Rust
normalization action, one 1 KiB no-op, one 1 MiB transfer, hostile loop/memory/trap fixtures; native
Linux/macOS/Windows and a documented browser backend attempt. Deliver build commands, exact
dependency versions/licenses, unsafe-free host diff, instruction/deadline/cancel evidence,
resource/latency reports through #151/#152 and a decision within a proposed five engineer-day
budget. Pass only with enforced memory and work limits, safe host API, native conformance and
no app crash/wedge; browser execution is separately gated on the same properties. If no candidate
passes, keep data-only packages and trusted statically linked adapters. If only browser fails,
ship native executable support with explicit browser UnsupportedCapability, never false parity.
A process fallback may be assessed in a separate two-day native-only subspike: prove OS sandbox,
termination and broker isolation on all three OSes or reject it; a worker process by itself fails.
Completion of #165 does not by itself authorize general Rust/Wasm package execution. If #165
selects a bounded expression interpreter, phase D may bind packages only to its versioned safe
definition format and update the illustrative manifest before freezing it. Arbitrary Rust-authored
Wasm package support remains a separately scoped phase D feasibility/security gate with the
same limits and native/browser evidence; a new runtime-selection issue is required if #165 did
not establish those properties. No second engine is built merely to satisfy this illustration. No production runtime selection is implied by these experiments. Numeric fuel calibration,
browser storage durability, signature algorithm/key revocation policy and provider broker field
allowlists are named deliverables of A/B/C/E, not unresolved assumptions hidden in an API example.

## 12. Acceptance map

| #154 requirement | Covered here |
|---|---|
| Use cases, scope and relation to existing systems | §1 |
| Compare implementations and choose staged path | §2, §11 |
| Rust/no-unsafe/layers/native/browser | §2–3, §11 |
| Identity, discovery, registration/schema, commands/jobs/errors | §4–5, §7 |
| Versions, dependencies, portability/missing data/migrations | §8–9 |
| Trust, rights/revocation, resources, paid safety | §5–7 |
| Packaging/development/update/integrity/license/registry boundary | §8 |
| Correctness/security/portability/performance validation | §10–11 |
| Architecture link, scoped decisions and claimable delivery issues | This RFC + architecture/decisions links; §11 phase issue links |
