# Project and asset storage

Native storage uses bounded streaming transfers and private disk-backed assets. This changes the
container and storage capabilities while preserving the current Comments-v2 and Tags-v3 graph
vocabulary. Historical v1/v2 inputs retain their contextual graph-role gates before typed decoding.

## Capabilities and versions

`Snapshot` carries a project, an `AssetStore` and an epoch. Native `ProjectStorage` advertises
`StorageCapability::StreamingV2` and supplies its authentic `AssetContext`. Session uses typed
`ArchivePlan`, `CheckpointPlan` and `FramePlan` methods throughout. Plans privately bind source,
context, immutable bytes and publication generation; callers cannot construct substitute plans.
The exact same prepared archive bytes feed Save and its recovery checkpoint. A published Save
with a failed checkpoint still reports `saved: true`, adopts its savepoint, and requires another
successful checkpoint before editing.

Native output selects current graph version v3 when the actual stored-ZIP layout fits every
small limit and uses no ZIP64 field or footer; otherwise it selects streaming container v4,
carrying the same current graph vocabulary. Both outputs stay sealed StreamingV2 plans;
byte-only defaults refuse them before I/O. Native checkpoints use the 80-byte NCRJ0004 envelope
with either embedded archive. Historical graph versions 1, 2 and 3 remain readable, including
Comments and Tags. The unmerged streaming-v2 prototype is not a supported on-disk format.

Small/default hosts emit current graph v3 and NCRJ0003 while retaining frozen v1/v2 read paths.
The buffered limits remain 16 MiB per asset, 48 MiB total, 64 MiB archive, 4 MiB manifest and
1,024 assets; frames are 64 MiB, journal 128 MiB. Compatibility defaults reject large/lazy plans
before reads or byte-method writes. Wasm retains this fallible capability boundary; a browser
streaming host is not supplied here.

Native container v4 admits canonical stored ZIP64: 8 GiB per asset, 64 GiB total retained logical
assets, 65 GiB archive, 4 MiB manifest and 1,024 assets. Directory metadata is at most 512 KiB and
1,025 entries including `project.json`. Compression, encryption, archive comments, directories,
symlinks, split archives, duplicate/overlapping/hidden extents and arbitrary names are rejected.
Only `project.json` and `assets/<lowercase BLAKE3>` regular entries are accepted. Local/directory
metadata, ZIP64 sentinels/extras/footer and CRC declarations must agree. V1 does not accept ZIP64.
Version admission precedes typed graph decoding. V1 and v2 apply their landed contextual
role gates; v3 and streaming v4 admit the current graph model. Classic NCRJ0001/2/3 recovery
replays against the original embedded archive hash. Native hosts checkpoint the recovered
project into NCRJ0004 before publishing new edits; a classic envelope is never parsed as v4.

The historical physical runs below used the unmerged prototype v2 headers. They establish
streaming mechanics and byte counts, not a fresh physical run of the v4 allocation.

## Lazy admission and verified consumption

An indexed `AssetDescriptor` is an unverified hash/length/CRC/range description. Index admission
checks graph/reference presence, not every body. Native Open copies the whole admitted archive
into a private immutable checkpoint source, rechecks its index/manifest against the admitted
snapshot, then installs the document. That portable transfer makes recovery self-contained after
external overwrite/deletion. It is not a zero-I/O or reflink guarantee.

`open_verified` grants a private immutable lease only after full length/BLAKE3/CRC checks and
available MIME/image metadata checks. Consumers stream through `VerifiedReader` or
`copy_verified_to`; `read_small` refuses above its 16 MiB cap. No graph field or descriptor alone
is content authority. Successful verification is cached only for the same live exact-length
identity, with bounded reclamation. Clones, readers, undo/redo and prepared imports share one
logical-retention ledger. Dead entries cannot accumulate uncharged body caches.

Image probing is bounded independently from full hash verification. JPEG header/orientation
metadata can be validated using a bounded synthetic header stream accepted by the pinned decoder;
it does not claim whole JPEG payload decode/validity. Unsupported metadata remains advisory;
corruption is never converted to a fabricated decoded image.

Card previews capture one opaque `PreviewAssetRead` for the exact asset identity. Capture does
not read, hash, probe, materialize, clone the complete store, or acquire the source-operation
lock on the UI thread. The native preview worker selects a pinned range under the existing
nonblocking operation authority, then reads at most the existing media decoder input limit
(256 MiB) in calls of at most 64 KiB, checking exact length, BLAKE3, indexed CRC when present,
and declared image facts before decoding. It never seals or packs unrelated assets and does
not publish global verification-cache state. The 16 MiB `read_small` helper is unchanged.
One active encoded buffer is owned by this path; queued jobs retain bounded capabilities,
not eagerly materialized bodies. Decoder/private workspace and allocator overhead are separate
owners, not a total-RSS promise. Cancellation is cooperative between reads; blocked foreign
reads and the active decoder cannot be forcibly interrupted.

Temporary source-operation contention releases the preview slot without caching a permanent
failure or scheduling a repaint. A later externally driven visible frame may retry; idle
eventual recovery is not promised. Actual missing identities and permanent verification errors
remain distinct. Without a worker, only insertion-verified resident inputs use the existing
small inline limits; a lazy input reports unavailable here rather than missing. The entire
selection/read/probe/decode attempt runs inside the preview panic guard.

Live and all undo/redo roots survive Save and `collect_assets()`. This host-only logical GC API
returns whether logical entries were removed, does not make a document edit/event/undo entry,
and does not promise immediate physical reclamation. Cleanup failure preserves logical state;
ordinary source destructors may attempt cleanup and retain charged debt. Recorded journal assets
are not removed by GC. History selection/navigation need not read inactive bodies; extraction
verifies selected payloads, while a broader restored AddGeneration validates its touched node.

## Imports, cancellation and ownership

`Session::prepare_imports` and native `prepare_reader_imports` mint one non-Clone opaque
`PreparedImports` batch. Whole-batch capability/count/counter/source-identity preflight occurs
before source reads. `dispatch_with_imports` accepts only an Edit against that exact candidate
store; stale/foreign receipts fail before handlers, services or file I/O. Preparation is not
publication. MIME/touched-reference validation still occurs during dispatch. Standalone imports
reuse the same receipt path. Candidate-existing duplicates may succeed at logical saturation;
identities pinned only by another reader/store are not that guarantee.

Known-length ranges hash caller input before candidate survival and compare the transfer hash.
Native unknown-length `import_reader` uses one 64 KiB buffer, stages/hashes until EOF, admits at
most 8 GiB plus a one-byte overflow probe and at most 1,024 total Interrupted results. Verified
and staging quota are reserved before each write. A private seal/adoption avoids rereading the
caller or routing through known-length import. Full unique candidate verification prevents a good
new spool from silently repairing a corrupt old indexed descriptor. Memory/wasm reject the reader
API before invoking it. Cancellation is checked between bounded work units; blocking caller
`Read` implementations must provide their own deadlines. There is no wall-clock interruption of
a blocked foreign reader.

The byte-input file/drop adapter uses one candidate batch and one `canvas.paste` Edit. Eligible
media is hashed in full; equal identities and lengths share one logical charge and one source.
Rejected paste leaves unrelated pre-staged assets and prepared authority intact. Native readers
borrow the input slices while staging; resident sources represent at most 48 MiB of unique input.
The adapter admits at most 32 node/source plans, while its ordered skipped-file results remain
proportional to the supplied file count, with bounded names and reasons prepared before dispatch.
Its 16 MiB per-input convenience cap and the UI's 48 MiB raw-read cap do not restrict the native
64 GiB project-store capacity. Raw UI reads still count duplicates. Blocking native acquisition
and hostile path substitution are separate unresolved work in [#335](https://github.com/CypherNaught-0x/nodecraft/issues/335).

Split into grid is an explicit bounded consumer of one selected image, including disk-backed
native input. It checks the real descriptor against the unchanged decoder input limit (256 MiB)
before source reads or buffer allocation, acquires an authentic verified lease, and fallibly
materializes that single input in reads of at most 64 KiB with exact length/EOF checks. At most
1,024 total Interrupted results are retried. The existing decoder's size, orientation and
allocation limits remain; this is neither whole-project eager loading nor a higher `read_small`
cap. The encoded source buffer ends before tile encoding. Codec internals and pixel/crop/RGBA/
encoder copies are separate phase owners, not a constant-RSS guarantee.

A split keeps at most 64 tile descriptors and one shared immutable encoded body per hash/length,
with at most 48 MiB of completed unique input bodies. A checked tile may temporarily overlap
its Vec-to-Arc conversion (at most 16 MiB); an oversized encoder temporary is refused under the
unchanged codec/accepted-tile policy. Resident preparation also owns its normal bounded candidate
and per-input copies separately. Native cursors and resident MemorySource wrappers share these
input bodies rather than retaining duplicate tile vectors. One prepared receipt and one paste
publish all tiles, using the host's retained logical count/byte limits. Existing candidate hashes
cost no new slot. The 48 MiB operation input table does not recap native project capacity.

`prune_assets()` remains an explicit compatibility name for guarded logical `collect_assets()`;
it is not import rollback. On import refusal, only non-logical cleanup maintenance may run.
A maintenance failure preserves the original error and adds retry guidance, with existing pins
and cleanup debt still charged. A successful committed paste is never changed into an error by
postpublication maintenance. Ambiguous persistence failure may require checkpoint repair even
though the live graph, history and logical asset map were not published.

Packing uses only already verified bytes and publishes cache relocation atomically. Old readers
retain their old ranges and charges. Old plus new disk/backing capacity is reserved before work;
slot exhaustion refuses safely rather than temporarily exceeding limits. Internal compaction and
metadata probes add reads beyond caller bytes; metrics and the physical harness report them.

## Disk, recovery and failures

These are logical disk reservations, not resident-memory or actual allocated-block measurements:

| Pool | Hard limit |
|---|---:|
| Verified bytes, including retained reader backings | 128 GiB |
| Private bases | 132 GiB; at most two base backings |
| Journals, including retired generations | 264 GiB |
| Serialized staging allowance | 66 GiB |
| Metadata | 1 GiB |
| Aggregate | 640 GiB |

There are at most 32 owned backings and 4,096 reservation/cleanup slots. Failed owned deletion
retains its charge and cleanup debt. No quota maximum is preallocated. A prepared archive is
promoted into a reserved base before a destination atomic stage uses the serialized staging
allowance. Pinned old generations can cause safe prepublication refusal.

NCRJ0004 begins with an 80-byte envelope: magic, archive length, archive BLAKE3 and a domain-bound
header digest. Checkpoint identity binds archive length/digest; every tail frame binds identity
and monotonic sequence. Frames are at most 65 GiB, tails 65 GiB, journals 132 GiB and record JSON
16 MiB. Distinct recorded asset identities/lengths stay within 1,024/64 GiB until a successful
checkpoint resets the ledger. Save after GC can reset it; failed Save cannot. Restored undo assets
absent from the new checkpoint are re-emitted in later frames.

Recovery rechecks copied checkpoint metadata/digest before state installation and verifies newly
persistent references. Proven stored body mismatches are corruption and require a fresh
checkpoint before append. Outer torn framing returns the validated prefix plus typed damage;
semantic violations are hard errors. Operational I/O/cancellation/quota and foreign association
errors are not classified by error message and do not blanket-latch all conflicts.

Native operation serialization uses a nonblocking try_lock: overlapping operations return
Busy rather than waiting; it does not move synchronous storage I/O off the UI thread. Save
may retain a private prepared base plus a same-directory publication stage alongside the old
file, and self-contained recovery keeps its own archive/journal owners. Streaming bounds
transfer buffers, not total disk copies; off-UI cancellation/progress and copy reduction remain
separate work.

Actual retained File handles, generation state, cooperative OS locks and the protected owned
namespace provide lifecycle authority. Same-file equality is a fallible mismatch diagnostic,
not a universal hostile-path substitution proof (including Windows/ReFS identity limitations).
Dirty host drop retains self-contained recovery and charges through live handles. Same-context
reopen reclaims its real owner; a distinct live context refuses rather than stealing it. Explicit
clean close/discard removes owned recovery only after synchronization. Atomic publication uses
same-directory stages; success means published, not immunity from hardware failure. Cleanup,
retired worker handles and failed removal remain charged until safely released.

## Private workspace crash maintenance

New native workspaces use `nodecraft-assets-v1-<pid>-<counter>` beneath the selected recovery
parent. The PID is naming only. A versioned, root-matched `workspace.v1.lock` is held by the
same workspace Arc as private source backings, ranges, readers and captures. File/cleanup permits
close before the final guard; dropping a storage host cannot authorize deletion of a retained
reader. Every constructor, including standalone `NativeAssetWorkspace::new`, coordinates through
a fresh exclusive lock on the permanent `.nodecraft-assets-v1.lock` parent file. Its bytes are
irrelevant and it is never truncated, renamed or removed. Same-parent admission is nonblocking;
different parents may proceed independently. No process-global registry or waiting loop is used.

Startup maintenance visits at most 4,096 parent entries, tries at most 64 qualified inventories
(including inventories later refused), and unlinks at most 256 generated payloads. Each orphan
requires an ordinary private directory, a separately opened exclusive marker lock and exact
bounded marker bytes. Windows marker bytes are read only AFTER locking; pre-lock checks use
metadata/type/reparse/path diagnostics and expected length. Symlinks and all Windows reparse
points are excluded. A complete flat preflight admits at most 4,096 `stage-N`, `archive-N` or
`capture-N` files plus the marker; unknown names, nested/nonregular entries, malformed IDs or
overflow refuse the WHOLE candidate before any payload deletion. Files are reinspected before
unlink. No payload bytes or hashes are read for maintenance, and no recursive removal is used.

Inventory entries contain only a fixed kind and canonical u64, without individual heap paths.
Actual admitted Vec backing is at most 512 KiB. The canonical parent and one reusable working
path are separately bounded to 64 KiB each; the native workspace's returned root remains its
existing lifetime owner. These are per active operation, not process-global/RSS, OS directory
buffer, filesystem latency or allocated-block guarantees. The 4,096 payload ceiling comes from
the existing cleanup-slot table, including failed-unlink debt; it is not the 32-backing limit.
Automatic constructor maintenance stops at its first classified old-candidate maintenance failure,
reports it in `retained_failures`, and creates a separate workspace if its own admission and
creation succeed. Successful partial deletions remain counted; available valid markers/payloads
are retained for later explicit retry. This incomplete pass reports `RetainedFailure`, without
claiming all entries were scanned. Public explicit sweeps, including `sweep_named_orphan`, still
return the original actionable error. Parent admission/identity, parent scan integrity,
cancellation, path/buffer/allocation invariants and new-root marker/lock/write/sync authority
remain fatal. No blanket error suppression, retry loop or extra deletion authority is added.
Admission stays held while the last marker handle closes and the marker/empty directory retire,
including on Windows. A failure after marker unlink leaves reported unmarked residue; it is not
silently adopted, recursively deleted or given replacement authority.

`NativeAssetWorkspace::maintenance_report` and `NativeStorage::workspace_maintenance_report`
expose bounded counts and scan/budget/failure flags, without paths or payload data. `Complete`
means the bounded pass/target finished, not that live or unqualified entries were removed. Exactly
filling the scan window conservatively reports incomplete without an extra directory read.
Directory order is unspecified: a stable 4,096-entry prefix can hide later roots forever.
`native_workspace::sweep_named_orphan(parent, basename, cancel)` is the bounded single-root
fallback, repeating the same authority/preflight checks without scanning the parent. It removes
at most 256 payloads per call (up to 16 batches for a 4,096-file root when IO succeeds). Basenames
cannot be paths. This library slice supplies no UI/CLI maintenance command or automatic retry
worker; host notification/routing is separate work.

Only the new protocol is automatically reclaimable. Legacy `nodecraft-assets-<pid>-<counter>`,
partial/unknown markers and unknown files remain untouched. Preupgrade residues need explicit
human inspection/removal with all old instances closed; no PID/age/size heuristic is authority.
Recovery journals/locks, original archives, Save destinations and external atomic stages are
outside the sweep namespace. NCRJ0004 retains complete checkpoint/frame bodies; tiny real crash
fixtures verify recovery after private orphan files are removed. Tests never sweep preexisting
user directories. The cooperative private-namespace contract excludes hostile same-user gate/
path replacement; metadata and same-file equality are mismatch diagnostics, not a universal
no-follow identity proof. Cancellation occurs between bounded units, not inside blocked OS IO.
These new tests do not repeat the historical large physical or hardware-power-loss evidence.

## Evidence and physical ZIP64 harness

Focused native tests execute real assets above 16 MiB and aggregate projects above 48 MiB, native
Session/history/recovery paths, quotas, cancellation and cleanup controls. Synthetic offset tests
cover hostile ZIP64 parsing; they are distinct from the physical run below.

On 2026-10-18, the two ignored physical tests passed sequentially on native macOS in the debug
profile: **2 passed, 0 failed**. Both generated a real 4,294,967,297-byte asset in bounded chunks,
without sparse files, reflinks or a giant buffer. The Session test completed full reader length/
BLAKE3/CRC validation, typed Save/Open/Save As, external input deletion after private snapshot,
large-checkpoint small-edit recovery, undo restoration and the last reader's lifetime after
Session/host destruction. The format test preserved complete old destination/recovery digest,
length and checkpoint identity on atomic failure, then passed checkpoint retry and cold
restart/torn-tail recovery.

| Observation | Measured result |
|---|---:|
| Large body, each physical test | 4,294,967,297 bytes |
| Later tiny entry's actual local-header offset | 4,294,968,519 bytes, above u32::MAX |
| Session archive / NCRJ2 checkpoint | 4,294,969,060 / 4,294,969,140 bytes |
| Atomic fault after successful forwarded file-write bytes | 4,294,967,296, above u32::MAX |
| Maximum individual source read | 65,536 bytes |
| Aggregate TransferBuffer peak, Session / format path | 131,072 / 65,536 bytes |

Raw local ZIP64 sizes, footer/directory metadata and the tiny entry's central-directory ZIP64
offset were checked against the actual file. Indexed Open had zero verified-body quota before
consumption. The fault left quota/backing counts unchanged and cleanup debt zero; both owned
fixture trees were removed before the next phase or test return. The accepted resource schedule
used actual A/J guards and a 60,297,314,314-byte conservative allowance, with refreshed free
space before each run; this is not a required reservation or a measured disk peak.

These are macOS results, not Windows/Linux physical execution. Successful forwarded file-write
bytes are not durable-device writes or a hardware power-loss test. The transfer peaks cover
two nested 64 KiB transfer buffers in the Session path; they exclude sniff prefixes, bounded
manifest/directory/JSON and graph/allocator/kernel memory, so they are not total RSS. Named-file
and allocated-block observations are snapshots, not transient or unlinked-open peak accounting.
The 121.49-second Session and 62.73-second format runs are individual validation timings, not a
benchmark or throughput guarantee. Full final CI and any current-main composition/review remain
required before publication; these two tests do not establish completion of every #147 gate.

See [development.md](development.md#physical-storage-validation) for exact opt-in commands. The
ignored large engine test fixes `L = u32::MAX + 2`, generates one bounded chunk at a time, finds
one later-sorting tiny hash with a bounded search, checks its actual offset, and streams a full
consumer hash/CRC. It uses public Session Save/Open/Save As, private source deletion, NCRJ2 small
edits/recovery, undo and a retained reader. A separate format unit test forwards actual atomic
file writes, injects failure only after successful bytes cross `u32::MAX`, and verifies complete
old destination/recovery preservation. These paths run sequentially with owned fixture cleanup.
Counters distinguish source reads, completed plan lengths, successful forwarded fault writes,
logical reservations and Unix allocated blocks; none alone measures device durability or RSS.
