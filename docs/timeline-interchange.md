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
| `timeline.export_interchange {canvas, id, path, overwrite?}` | UI, CLI, control channel | Writes the archive to an absolute `path`. Desktop only. Not offered to MCP agents, because it writes a file on this machine. |

In the app, open the Timeline editor and choose **Export for another editor…**. Neither command
opens a dialog: the UI asks where to save first and then dispatches the command with the path.

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
| Fade in/out | **not written**, reported | A follow-up can translate fades into keyframed volume and opacity. |
| Background colour | **not written** unless black, reported | Editors show black behind a picture that doesn't fill the frame. |
| Fit inside the frame | not written | Editors fit a clip of another size by default, as the render does. |
| Disabled or unconnected clips | left out, reported | |
| Captions | connected `<title>` elements plus a UTF-8 SRT companion | Text and line breaks, frame-rounded start and length. Captions are clipped to the exported sequence and never lengthen it. Disabled and out-of-range captions are left out and reported. |
| Caption alignment and position | text alignment and frame anchors in FCPXML; absent from SRT | Distinct lanes above picture tracks preserve overlapping titles. The destination's Basic Title template can change exact margins and text metrics; every caption reports this. |
| Caption font, size, colour and outline | destination Basic Title appearance; absent from SRT | The requested font, pixel size, fill and outline are listed in the report, so style is never silently lost. No Motion template or font is included. |
| Clip placement, speed and effects | not yet translated by this writer | Separate from caption interchange. |

The report lists each loss, naming the clip it concerns. The document never claims more than it
holds, and the export never presents itself as a lossless round trip.

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
- At most 256 media files go in one archive. Entry names are at most 255 bytes, relative, and
  free of `..`, backslashes, colons and control characters. Names that differ only in case are
  refused. The document and report are capped at 16 MiB each.
- The export runs on the thread that dispatches it, like `history.export`. Moving it to a
  cancellable background job is tracked in #643.

## Compatibility

Unit tests check each document's elements and timing, frame by frame. Nothing validates it
against the FCPXML DTD yet, and importing it into Final Cut Pro or DaVinci Resolve **has not been
verified**. Please report what an
editor does with an export, and which version of the editor you used.
