## 2026-10-21 · Timeline clips carry their place in the frame, their speed, and the render a quality (#577)

This extends [the Timeline record](2026-10-21-timeline.md), [its tracks](2026-10-21-timeline-tracks.md)
and [its captions](2026-10-21-timeline-captions.md); their storage, rendering and limits still hold.

- **Flat fields on the clip, in thousandths.** A picture clip's place in the frame is `pos_x`
  and `pos_y` (its centre, in thousandths of the frame, -1000..=2000 so it may sit partly
  outside), `scale` (thousandths of the size that fits the frame, 10..=4000), `crop_left`,
  `crop_top`, `crop_right` and `crop_bottom` (thousandths of the picture; opposite crops leave
  at least a tenth), `rotation` (whole degrees clockwise, -180..=180) and `opacity` (0..=1000).
  Each is left out of the stored clip at its default, so older timelines store and render
  exactly as before. Sounds take no place in the frame, and a clip that sets one is refused.
- **Crop, then fit, then size, then turn.** The render crops the picture, fits what is left to
  the frame, sizes it, turns it about its centre (the corners outside it are transparent),
  applies its opacity and draws it centred where it goes over the background or the track
  below. A clip at the identity placement keeps the old fit-and-pad path.
- **Speed is media time over output time.** `speed` is thousandths (250..=4000; 2000 plays twice
  as fast, and sound keeps its pitch through chained `atempo`). `in_us` and `out_us` stay times
  in the clip's own media; fades, cross-fades, `at_us` and the clip's length on the timeline are
  output time. A still has no speed.
- **Quality is a parameter of the card.** `quality` is `draft`, `standard` (the default) or
  `high`, chosen in the inspector or the editor's output frame panel; it picks the VP9 constant
  quality and bit-rate cap, and nothing else about the output.
- **The editor shows it as it renders.** Drags and trims move through the media at the clip's
  speed and catch on other clips' edges, the playhead and the output's start (Alt lets go),
  else on a whole frame; the selected picture's place is outlined over the preview.
