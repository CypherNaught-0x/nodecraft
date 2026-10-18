# Native video service

With the optional `nodecraft-media/video` feature, `video::VideoService` imports bounded media,
decodes preview frames, supports latest-request-wins scrubbing, and encodes a stream of RGBA
frames to VP9 WebM. This is a usable library service, not a timeline UI or an engine command.
It does not call providers. The default media build is unchanged.

## Tools and platforms

Configure absolute paths to your installed **ffmpeg** and **ffprobe**, plus a trusted private
scratch parent. NodeCraft does not download, package or redistribute either tool. Their build
configuration determines codec availability and licensing; invoking them is not a certification
of their licence. The service requires the tested MOV/Matroska demuxer controls, H264/VP8/VP9
decoders, libvpx-vp9 encoder and colorspace/scale/transpose filters. Unsupported configurations
fail explicitly. Never take executable paths or command options from a project.

The native backend uses bounded worker/pipe threads and ordinary processes. It does not sandbox
a malicious executable, codec vulnerability, descendants or external process memory. Standard
Flatpak installations do not grant host executable access; an explicitly configured permitted
sandbox executable is necessary. There is no host-spawn escape or broader Flatpak permission.
The browser API rejects setup as Unsupported before native work; WebCodecs is separate.

## Nonblocking host integration

`ToolPaths::new` and `ServiceConfig::new` validate syntax only. `begin_open` starts bounded
background initialization; poll `VideoOpening::try_result`. Filesystem checks, capabilities,
source hashes, staging, probe, decode and encode happen on the worker. Thread creation itself
has no hard real-time guarantee. Never wait or run an external tool in a UI frame.

Import accepts an immutable Arc byte extent and an explicit `ColorPolicy`. On refusal it returns
the source. Assets have private construction and immutable metadata/hash getters. They pin their
staged file and service budget; clones share the same pin. `frame` returns a pollable job.
`FrameRequest` times are microseconds relative to the selected video stream. The operational seek
normalizes format/stream start times, then uses output-side seeking. Returned metadata identifies
the requested time, not an invented exact PTS or nearest VFR frame. End-of-stream yields NoFrame.

Preview geometry respects SAR and exact supported quarter-turn display matrices. FFprobe display
matrix rotation is counterclockwise; internal rotation is clockwise. Autorotation is disabled
and the normalized transform applied once. Mirrored/nonorthogonal/conflicting matrices and legacy
rotate tags without a verified interpretation are Unsupported. Output is square-pixel RGBA8.

Default `AssumeBt709ForUnspecified` fills absent SDR tags with a documented effective profile;
`assumed_fields` and original/effective color information make this visible. `RequireDeclared`
refuses missing properties. Finite BT709/601 profiles are supported; explicit HDR/wide-gamut,
unsupported bit depths, alpha-video and interlacing are rejected. No HDR/ICC fidelity claim.
Conversions use fixed colorspace filters to sRGB, not just metadata retagging.

`ScrubSession` cancels old work and exposes only the latest requested result. A bounded 256-entry LRU cache
shares immutable pixels; caller-held frames remain globally charged after cache eviction. The
fallibly preallocated registry and cache container capacities remain charged in the service base
lease even when entries are vacant; the 64 MiB cache sublimit also includes frame capacities and
per-entry metadata. Eviction releases one entry outside its short cache lock, without a temporary
eviction buffer.

## Render and artifacts

Construct `RenderSpec` with even dimensions, rational frame rate, count and an opaque background.
`FrameSender::try_send` accepts exactly sequenced RGBA frames and returns the same packet on
backpressure/refusal. Retry Busy after a host timer; do not spin. Finish only after the exact
count. Dropping an unfinished sender cancels. There is one admitted render at a time: admission
reserves the two queued frames, one writer-owned frame and RGB conversion workspace before
returning the sender. Successful `finish(self)` consumes sender ownership. A sender retained after
cancellation or a terminal error keeps that reservation and render slot until dropped; operation
and packet owners also retain it through acknowledgement. Alpha is composited against the specified
background in linear sRGB before converting to opaque BT709 VP9; transparency is not discarded
silently or falsely preserved by yuv420p.

There is one POST-free encode process, not one process per frame. Completion requires successful
write/exit, strict final size and a probe verifying the actual decoded frame count and geometry.
A partial valid file, a quota-truncated file or exit zero alone is not success. Results retain
private files, hash and metadata. Read bounded chunks asynchronously with `read_chunk`; no scratch
path is exposed. The example explicitly creates a new destination file and exports the chunks.

```sh
cargo run -p nodecraft-media --features video --example video_roundtrip --   /absolute/ffmpeg /absolute/ffprobe /new/output.webm
```

## Limits, cancellation and cleanup

Each service has one cleanup coordinator and two operation workers, with at most two direct
children and three pipe workers per child (nine service threads in total). Decode/read work can
therefore supply an active render from the same service. No registry/cache lock is held across
filesystem I/O, child execution, joins or queue waits. Shutdown drains queued cancelled work,
including after worker failure, so queued asset pins cannot strand cleanup. Poisoned metadata
locks fail new operations closed while cleanup recovers ownership.

The absolute operation deadlines include queue time and all staging/probe/hash/publication
stages: startup 10 seconds, import 10 seconds, preview 15 seconds and render 600 seconds. Stages
never renew a deadline. Uninterruptible filesystem/kernel calls are not a hard real-time guarantee.

Per service: 512 MiB logical retained memory, 1 GiB reserved scratch, 64 MiB cache sublimit,
32 jobs and 1024 asset/artifact records. Lower-only limits may be injected. Queued input owners,
results, cache metadata, frame queues, capture/parse/codec scratch and externally pinned files
stay charged. This is not allocator-exact RSS; host-owned copies and multiple services require
host admission. Arbitrary Arc owners expose length, not allocator capacity. Rendering at all
individual maxima together may fail the aggregate budget.

Processes use fixed arguments, input formats, local/pipe protocol allowlists, disabled MOV external
references, stripped environment and private files. No shell or supplied filters/URLs are used.
External-tool memory and disk-write overshoot are not OS quotas. `-fs` is secondary; exceeding
strict final size never publishes an artifact. Network and unrelated local-file loading are not
permitted by the supported format/options contract, but this is not an OS sandbox theorem.

Cancel/drop is a signal, not a blocking join. The supervisor can kill/reap a direct child while
pipe workers are blocked. Terminal cancellation follows direct-child and pipe-worker acknowledgement; file cleanup is separate. `begin_shutdown` closes
admission; `ShutdownTicket` reports children stopped separately from externally pinned files.
A timed-out blocking wait returns the ticket. Last-pin drop does no filesystem/process/join work;
the existing supervisor scans the bounded weak registry and unlinks files. Notifications/Arc
release are not lock-free or hard-real-time promises. Failed unlink retains its quota and cleanup
owner, retrying with exponential backoff from 100 milliseconds to 5 seconds. A host retaining pins forever retains one bounded service worker; release pins and await
final cleanup before replacing services without bound.

## Validation

Normal feature tests use Rust subprocess fixtures, with no installed tool or download required.
Real integration tests require explicit tool paths and run authored tiny media only:

```sh
NODECRAFT_VIDEO_TEST_FFMPEG=/absolute/ffmpeg NODECRAFT_VIDEO_TEST_FFPROBE=/absolute/ffprobe cargo test -p nodecraft-media --features video actual_ -- --ignored
```

These tests cost no provider budget. Native runtime proof and wasm compilation are distinct;
Windows/Linux/browser behavior must not be inferred from a macOS run. No binary fixtures are
redistributed. No speed, full codec support or fully decoded security guarantee is implied.
