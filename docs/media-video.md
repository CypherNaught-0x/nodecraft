# Native video service

With the optional `nodecraft-media/video` feature, `video::VideoService` imports bounded media,
decodes preview frames, supports latest-request-wins scrubbing, and encodes a stream of RGBA
frames to VP9 WebM, and stitches staged clips into one WebM (see "Compose" below). The desktop
app uses it to play videos on cards (see "On cards" below); the service itself is not an engine
command. It does not call providers. The default media
build is unchanged.

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

## Compose

`VideoService::compose(ComposeSpec)` renders a timeline of staged clips in one FFmpeg run and
returns the same verified `RenderedVideo` as a render. It shares the single render slot.

- **Clips.** `ComposeClip::new(asset, in_us, out_us)` takes a span of an imported asset. Add
  `with_fades` (to and from the background colour, and silence), `with_volume` (thousandths, up to
  4.0×, or muted) and `with_transition` (a cross-dissolve of picture and sound into the next clip,
  at least 10 ms).
- **Spec.** `ComposeSpec::new` uses the frame rules of `RenderSpec`. It takes 1 to 32 clips and at
  most an hour of output. Every clip must show for at least one frame outside the transitions it
  takes part in, and the last clip has no transition.
- **Picture.** Each clip is converted from its declared colour to BT.709, rotated, and fitted
  inside the frame at its display aspect, with letterbox bars in the background colour. It is then
  resampled to the output rate and padded or trimmed to exactly its span. Cuts concatenate;
  transitions use `xfade` at the running length.
- **Sound.** Only the first AAC, Opus, Vorbis, MP3 or FLAC stream of a clip is used, resampled to
  48 kHz stereo. A clip with no usable audio, or a muted one, contributes silence, so the output
  always has an Opus track. Transitions use `acrossfade`.
- **Capabilities.** On top of what opening the service checks, compose needs the `xfade`,
  `acrossfade`, `tpad` and `anullsrc` filters, the `libopus` encoder, and a decoder for each audio
  codec in use. It fails as Unsupported without them.
- **Verification.** The output is probed for its size, rate, audio, frame count and running time
  (each within two frames) before it is accepted.
- **Command line.** Every number on it is formatted from validated integers. No project or caller
  string reaches FFmpeg.

## Video actions

The video action nodes (#108) are ordinary action kinds (`crates/nodes/src/video_edit.rs`) whose
executor (`crates/engine/src/action_exec/video.rs`) builds one `ComposeSpec` and renders it like
the Timeline does, on the same shared service, render slot, limits and cancellation. Each makes a
new WebM; the connected videos are never changed. The output keeps the first video's upright
shape (sample aspect and rotation applied, fitted within 4096 a side, even sides) and its frame
rate when the renderer takes it, else 30 a second.

| Kind | Inputs | Does |
|---|---|---|
| `video.trim` Trim video | a video | Keeps `[start, end)`; an end of 0 is the video's end. |
| `video.speed` Change speed | a video | 0.25× to 4×; the sound keeps its pitch (`atempo`) or is silenced. |
| `video.stitch` Stitch videos | 1 to 32 videos | Plays them in the order connected, cut or crossfaded. |
| `video.replace_audio` Replace sound | a video and a sound | Replaces or mixes the video's sound with the new one, from an offset, at a volume. The output is the video's length: a longer sound is cut at its end, a shorter one leaves silence. |

Every kind has a Quality choice (Draft, Standard, High). Settings are checked before any video is
imported, so a bad trim or too many stitched videos costs no import. Without `[media]` tools the
actions say so; in a browser they say they need the desktop app. Reverse, frame and sound
extraction, contact sheets, grading and the other operations #108 lists come in later parts.

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

## On cards

The desktop app compiles the service in and plays a video on its card: a `source.video` node's
asset, or the first `video/*` result of a generation card. Set both tools, as absolute paths, in
the settings file:

```toml
[media]
ffmpeg = "/opt/homebrew/bin/ffmpeg"
ffprobe = "/opt/homebrew/bin/ffprobe"
```

They come from settings only, never from a project. Without them, or in the browser build, the
card says what is missing instead of a frame. A card shows a poster frame from the start of the
clip, a play/pause button, a position slider and the time. Playing and scrubbing are view state:
they change nothing in the document and are not commands.

Each frame is one ffmpeg run, so playback shows the newest frame the tools can deliver rather
than every frame at the clip's rate: each card has at most one frame in flight and drops the
times it fell behind on. At most four frames, two imports and one asset read run at once, and
staged clips stay under 768 MiB (least recently used are evicted). Posters are cached per
content hash and size, up to 64. Only cards on screen ask for frames.

## Validation

Normal feature tests use Rust subprocess fixtures, with no installed tool or download required.
Real integration tests require explicit tool paths and run authored tiny media only:

```sh
NODECRAFT_VIDEO_TEST_FFMPEG=/absolute/ffmpeg NODECRAFT_VIDEO_TEST_FFPROBE=/absolute/ffprobe cargo test -p nodecraft-media --features video actual_ -- --ignored
NODECRAFT_VIDEO_TEST_FFMPEG=/absolute/ffmpeg NODECRAFT_VIDEO_TEST_FFPROBE=/absolute/ffprobe cargo test -p nodecraft-ui-egui actual_video -- --ignored
```

The second drives a real clip through a card: play, the end, and a seek.

These tests cost no provider budget. Native runtime proof and wasm compilation are distinct;
Windows/Linux/browser behavior must not be inferred from a macOS run. No binary fixtures are
redistributed. No speed, full codec support or fully decoded security guarantee is implied.
