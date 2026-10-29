# Timeline interchange

A Timeline node's clips can be exported so another editor can keep working on the cut without
everything baked into one video (#166). The export **renders nothing and calls no provider**.
It writes one ZIP archive:

```text
<timeline name>.fcpxml     the cut, FCPXML 1.10
<timeline name>.srt        caption text and timing, when at least one caption shows
report.txt                 what the document leaves out or changes, for a person to read
media/01-<source>.<ext>    each source the clips play, once
media/02-<source>.<ext>
```

The document names each file by its path relative to itself (`media/01-Fox.mp4`,
percent-encoded), and nothing in it points outside the folder. Unzip the archive anywhere and
open the `.fcpxml`. Whether every editor resolves a relative `src` this way **has not been
verified** yet (#643).

## Commands

| Command | Who | What |
|---|---|---|
| `timeline.interchange {canvas, id}` | UI, CLI, MCP, agent | The document, its media list and every loss, without writing anything. |
| `timeline.export_interchange {canvas, id, path, overwrite?, background?}` | UI, CLI, control channel | Writes the archive to an absolute `path`. Desktop only. Not offered to MCP agents, because it writes a file on this machine. |

In the app, open the Timeline editor and choose **Export for another editor…**. Neither command
opens a dialog: the UI asks where to save first and then dispatches the command with the path.
The UI requests `background:true`: the canvas stays usable and its export progress area has a
**Cancel export** button. Quitting asks whether to keep exporting or cancel and wait for cleanup.
Programmatic callers retain the existing synchronous default (`background:false`), whose reply
is `{bytes}`. With a host export runner, `background:true` instead first replies `{pending:id}`;
`export.status` reports its phase, media/byte progress and terminal result, and `export.cancel {id}`
cancels before publication. The final successful result is `{state:"exported",bytes}`. The initial
receipt never claims bytes have already been saved. A missing/refused runner is an error; no
background request silently falls back to blocking the caller.

## Which clips and media are written

- **Clips** are the node's saved clips, plus a whole clip for each wired video, sound or picture
  that no saved clip names. This is the list `timeline.list` shows and a render plays.
- **Media** is what each wired node shows now: a source node's file, or the item of the upstream
  node's active result. Choosing another result changes what is exported, just as it changes what
  a render uses.
- **A wired node with no result yet** stops the export with an error naming the clip. Run that
  node or disconnect it. The export never writes a document with holes.
- **A clip that doesn't fit its media** (it ends after the media does, or starts after the media
  ends) and a video or sound whose length is unknown with no end set stop the export too, naming
  the clip.
- **Unconnected and disabled clips** are left out and listed in the report.
- **Repeated sources are written once.** The same file wired twice, or played by several clips,
  is one file in `media/` and one `<asset>` in the document. Files are numbered in the order they
  are first wired. Each name comes from the source node's title, keeping letters and digits.
- **Only the media the document names is in the archive.** Other project assets, the project
  file, settings, keys and account details are never written. All media lives in the project's
  asset store; there are no externally linked files.

## Format choice

| Format | Tracks | Trims | Transitions | Audio level | Rates | Relative media | Verdict |
|---|---|---|---|---|---|---|---|
| CMX3600 EDL | one video and up to four sound channels per event list | yes | dissolve and wipe by number | no | 24/25/30 (29.97 drop-frame) | reel names only, no paths | Too narrow: no layering beyond V1, and media is matched by reel name rather than file. |
| FCP7 XML (xmeml) | many | yes | yes | yes | yes | `pathurl`, usually absolute `file://` | Widely read, but legacy, and editors resolve relative `pathurl`s inconsistently. |
| **FCPXML 1.10** | spine plus connected clips on lanes | yes | dissolves as `<transition>` | `adjust-volume` | rational frame durations | `media-rep src` as a relative URL (resolution against the document unverified) | **Chosen.** Read by Final Cut Pro and DaVinci Resolve. Media is referenced relative to the document. |

Other formats (an EDL for conform, OTIO) can be added beside it later. The engine commands
already report the `format` and `version` they wrote.

## Feature matrix

| NodeCraft setting | In the document | Notes |
|---|---|---|
| Frame size | `<format width height>` | exact |
| Frame rate (24, 25, 30, 50, 60) | `frameDuration="1/N s"`, every time a whole frame | The Timeline offers whole rates only, so there are no fractional (23.976, 29.97) rates to translate. A time that falls between frames is rounded to the nearest one and reported. |
| Main track order | spine `asset-clip`s in order | exact |
| Trims (in/out) | `start`, `duration` | to the frame |
| Cross transition into the next clip | `<transition name="Cross Dissolve">` centred on the cut | Each clip lends half of the overlap to the transition, the way FCPXML expects. The total length matches the render. |
| Picture tracks over the main track | connected clips on lanes 1, 2, … in track order | Attached to the main-track clip under their start, or to a trailing gap past its end. |
| Sound tracks | connected clips on lanes -1, -2, … | as above |
| Stills | `<video>` held for the clip's length | |
| Volume (thousandths) | `<adjust-volume amount="… dB">` | Converted to decibels. Volume 0 and muted clips become -96 dB. |
| Video with its own sound | `hasAudio` on the asset, sound played with the clip | From the asset's channel count. |
| Fade in/out | Keyframed `adjust-blend` and `adjust-volume` | Picture opacity has linear frame-aligned corners. Audio samples the linear amplitude envelope at every sequence frame before converting to dB. Muted/zero-volume audio stays at -96 dB. Actual editor interpretation remains unverified. |
| Clip opacity | `adjust-blend amount` and the fade peak | Thousandths become 0–1; zero stays transparent. |
| Background colour | **not written** unless black, reported | Editors show black behind a picture that doesn't fill the frame. |
| Fit inside the frame | not written | Editors fit a clip of another size by default, as the render does. |
| Disabled or unconnected clips | left out, reported | |
| Captions | connected `<title>` elements plus a UTF-8 SRT companion | Text and line breaks, frame-rounded start and length. Captions are clipped to the exported sequence and never lengthen it. Disabled and out-of-range captions are left out and reported. |
| Caption alignment and position | text alignment and frame anchors in FCPXML; absent from SRT | Distinct lanes above picture tracks preserve overlapping titles. The destination's Basic Title template can change exact margins and text metrics; every caption reports this. |
| Caption font, size, colour and outline | destination Basic Title appearance; absent from SRT | The requested font, pixel size, fill and outline are listed in the report, so style is never silently lost. No Motion template or font is included. |
| Clip speed | **not written**, reported when not 1000 (1×) | Exports the unretimed length derived from media trim; retime in the destination editor. |
| Clip position (`pos_x`, `pos_y`), scale, crop edges, rotation | **not written**, reported for each changed field | Exports centred, at the editor's default fitted size, uncropped and upright. Position axes and crop edges are grouped within their respective feature but individually named. |
| Effects | not written | |

The report lists each loss, naming the clip it concerns. The document never claims more than it
holds, and the export never presents itself as a lossless round trip.

Fade times use the clip's source clock, including trimmed media starts and dissolve handles.
Fades are specified in played time in NodeCraft: for the still-unsupported speed setting, their
positions scale into source time with the unretimed exported clip. The speed loss remains in the
report. Frame rounding, including fade boundary rounding, is reported. Audio's dB interpolation
between frame samples may differ from the native sample-by-sample amplitude ramp; each audible
fade records an `audio fade interpolation` report entry. Matching the
curve in both destination editors is part of the outstanding import validation below.

Caption losses name the caption id. SubRip times are rounded to the nearest millisecond and
sorted by start time. Empty caption lines contain a zero-width space so they cannot end a cue;
literal HTML punctuation is escaped. These changes are reported. SRT has no portable style,
box or position information. It accompanies the current FCPXML export and can be imported
separately; there is no EDL export mode yet.

## Safety and limits

- `path` must be absolute and end in a file name. Errors never repeat it.
- An existing file is only replaced with `overwrite`. The archive is staged next to its target and
  appears only once it is complete, so a failure, including a full disk, leaves nothing half
  written.
- Every source is checked against its content hash as it is copied into the staged archive. Entries are stored, not recompressed, and copied as streams.
- At most 256 media files and 8 GiB of media go in one archive. Entry names are at most 255 bytes, relative, and
  free of `..`, backslashes, colons and control characters. Names that differ only in case are
  refused. The XML, report and SRT are capped at 16 MiB each. A document admits at most 65,536
  fade keyframes; shorten long sound fades or lower the frame rate if that bound is reached.
- Background admission captures only bounded clip/source metadata. XML is built on the worker.
  Each indexed source is privately verified into managed workspace scratch, then copied directly
  into the staged ZIP and released before the next source. At most one source's scratch is live;
  it shares the native workspace quota. Sources are never all pre-staged in the system temp folder.
- Cancellation is checked while building keyframes, writing sidecars and copying media. It waits
  for an in-flight filesystem call to return. Once **Finishing export…** appears, syncing and atomic
  publication cannot be cancelled. A cancelled pre-publication job removes its owned stage and
  preserves an existing destination. Forced process termination can leave an atomic writer stage;
  ordinary cancellation and quitting wait for cleanup.
- Edits after background admission affect later exports. The current export keeps the captured
  clips, titles and source bytes. Opening/replacing the session cancels its job; a retained receipt
  remains observable until the worker has released its files.

## Compatibility

Unit tests check each document's elements and timing, frame by frame. Nothing validates it
against the FCPXML DTD yet, and importing it into Final Cut Pro or DaVinci Resolve **has not been
verified**. Please report what an
editor does with an export, and which version of the editor you used.

### Required editor import evidence for #643

The offline changes are **draft-only until both rows below have real results**. Structural XML
tests and opening ZIP entries are not import proof. No editor version is inferred from an export
file. On the development host, neither Final Cut Pro/Trial nor DaVinci Resolve/Studio was found
in installed application metadata; no editor was installed or launched for these checks.

| Editor | Version | Representative import | Relative media, fades, lanes, transitions, captions | Status |
|---|---|---|---|---|
| Final Cut Pro | unavailable | not run | not checked in the editor | **merge blocker** |
| DaVinci Resolve | unavailable | not run | not checked in the editor | **merge blocker** |

The original tiny moving-marker video fixture has twelve distinct frames and a 440 Hz tone;
the sample author adds original WAV/PNG content. The ignored sample-export test writes portable
ZIPs to `NODECRAFT_INTERCHANGE643_SAMPLES` for a person with both editors. Import each unzipped
document, inspect media relinking, trim/transition handles, connected picture/audio tracks,
opacity and audible ramps, then record the exact app versions, results and any required fixes
here before marking the PR ready. No copyrighted Apple DTD is vendored: its public documentation
is not a permissive redistribution license, and even DTD validation would not prove an import.
