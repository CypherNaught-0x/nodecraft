## 2026-10-21 · Timeline captions are stored with the clips and drawn by the app (#574)

This extends [the Timeline record](2026-10-21-timeline.md) and
[its tracks](2026-10-21-timeline-tracks.md); their storage, rendering and limits still hold.

- **Captions live with the clips.** A Timeline's `captions` sit next to `clips` and `tracks` in
  `config["nodecraft.timeline"]`. They are left out when empty, so a Timeline without captions
  stores exactly what it did before and older projects open unchanged. A project with
  captions doesn't open in a build from before them. Each caption has an id,
  text, a start and end time on the output, and a style: font, size in output pixels, `left`,
  `center` or `right` across, `top`, `middle` or `bottom` up and down, a fill and an outline
  colour, and an outline width. A disabled caption stays listed but isn't drawn.
- **Bounded.** A timeline holds at most 64 captions. A caption holds at most 400 bytes and 8
  lines; lines break only at newlines. Its size is 8 to 400 pixels, its outline 0 to 32 pixels,
  and its times fall within the Timeline's hour. The stored document's cap rose from 64 to
  128 KiB to fit them.
- **Commands, one undo step each.** `timeline.caption_add`, `timeline.caption_update` and
  `timeline.caption_remove` change one caption. `node.timeline_set` replaces them all when it
  is given `captions`, and keeps them when it isn't. `timeline.list` lists them.
- **The app draws the text, not ffmpeg.** Each caption is laid out with the Layer Editor's text
  code (so a font the app doesn't have falls back to the bundled Lato) and drawn into a picture
  only as large as the text. That picture reaches ffmpeg as raw pixels, as stills do, and is
  laid over the finished picture between its times. So ffmpeg needs no `drawtext` filter or
  fonts, and the render looks the same on every machine. Captions sit a twentieth of the frame
  from the edge they are aligned to. The outline is drawn under the fill and reaches as far
  inside each letter as outside it, so a see-through fill shows the outline at its edges.
- **Captions don't lengthen the output.** One that starts after the output ends is left out,
  and one that runs past the end is cut there. Times land on the output's frames.
