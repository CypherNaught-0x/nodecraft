## 2026-10-22 · Interchange captions are connected titles plus a SubRip companion (#649)

The existing interchange format remains FCPXML 1.10; no EDL writer exists yet. Captioned exports
add a same-stem UTF-8 `.srt` beside that document, `report.txt` and the media. The preview command
returns that companion's name and text too. Both text entries use the archive's existing size,
relative-name and case-insensitive collision checks.

The pure `nodes` writer rounds and clips captions to the exported sequence's frame grid, with
at least one frame, and reports rounding, clipping, disabled and out-of-range captions. Titles
attach in their parent's media time, on distinct positive lanes above every picture track, so
overlaps preserve the Timeline's stacking order. SubRip sorts the same captions by their start
time and rounds frame boundaries to milliseconds (at least one millisecond), reporting that change.

FCPXML titles reference the destination editor's Basic Title Motion template; NodeCraft does not
copy or bundle a Motion template. Text and line breaks survive XML escaping. Horizontal text
alignment and frame anchors use FCPXML's frame-height percentage translations, with positive y
up. These anchors do not promise the same font metrics or exact 5% margins. Every caption's
report names the font, pixel size, fill and outline left to the destination template, and the
placement differences. SRT carries no style, box, alignment or placement.

Empty caption lines contain a zero-width space in SRT so they cannot terminate a cue. Literal
HTML punctuation is escaped because SubRip readers may interpret markup. Both adjustments are
reported; the FCPXML retains the original text except for XML 1.0 noncharacters, also reported.

The element and adjustment contracts are in Apple's [FCPXML 1.10 DTD](https://developer.apple.com/documentation/professional-video-applications/document-type-definition)
and [transform reference](https://developer.apple.com/documentation/professional-video-applications/adjust-transform).
The [Library of Congress SubRip description](https://www.loc.gov/preservation/digital/formats/fdd/fdd000569.shtml)
documents its cue separators, millisecond timecodes and reader-dependent markup. DTD validation
checks structure; actual import into Final Cut Pro or DaVinci Resolve remains a separate
compatibility check, as for the existing interchange writer.
