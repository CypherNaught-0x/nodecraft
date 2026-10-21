## 2026-10-21 · Timeline tracks hold sounds and pictures at their own time (#576)

This extends [the Timeline record](2026-10-21-timeline.md); its storage, rendering and limits
still hold.

- **Track 0 is implicit and plays in order.** The main track is the old single track: videos and
  stills one after another, with cross-fades. It isn't stored; `tracks` lists only tracks 1 and
  up, at most 8 of them, each `video` or `audio`. A Timeline with no extra tracks stores exactly
  what it did before, so older projects open unchanged. A card saved before this change has only
  its `clips` input. It still runs, and wiring a sound or picture into it adds the two new inputs
  in the same edit (the kind's migration, applied by `edge.connect`).
- **Other tracks place clips in time.** A clip off the main track starts at `at_us` on the
  output. Picture tracks draw over the main track (higher tracks on top) and sound tracks are
  mixed in. The output runs until the last clip on any track ends, and the main track holds its
  background colour until then. Only the main track has transitions; elsewhere, use fades.
- **The input decides what a clip is.** The card takes videos on `clips`, sounds on `audio` and
  pictures on `stills`. Each clip records which input it came from (`input`, default `clips`).
  A sound goes only on a sound track, and a video or still only on a picture track or the main
  track. A still has no media length: its `out_us` is how long it shows (3 s by default). It
  isn't trimmed at the start and isn't split. Automatic ids keep the old `{origin}-{nth}` form
  for videos, and add `-a` and `-s` for sounds and stills.
- **A wired sound gets a sound track of its own.** It goes on the first sound track, which is
  added if the Timeline has none (when there is room). If all 8 extra tracks are picture tracks,
  the run refuses rather than drop the sound unheard. Wired videos and stills go on the main
  track.
- **Tracks are commands too.** `timeline.track_add` appends a track. `timeline.track_remove`
  refuses the main track and any track that still holds clips; removing a track renumbers the
  tracks above it. Moving a clip onto the main track clears its `at_us`, and moving it off the
  main track clears its transition.
- **Stills reach ffmpeg as raw pixels.** The renderer decodes each still with the app's own
  image decoder and gives ffmpeg raw RGBA. So, as with the tools themselves, a project can't
  make ffmpeg parse an image file it supplies. On the main track a still is laid over the
  background colour, so its transparent pixels show the background. On a picture track it keeps
  its transparency.
