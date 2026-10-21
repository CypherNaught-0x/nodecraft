## 2026-10-21 · Videos play on cards through settings-configured FFmpeg (#105)

- **Tools come from settings only.** `[media] ffmpeg` and `ffprobe` must both be absolute paths;
  a project never names an executable. Finding them on `PATH` or bundling them is #411.
- **Playback is view state.** Play, pause and the position live in the shell per node, like pan
  and zoom; they are not commands and never touch the document or undo.
- **One frame in flight per card.** Each frame is one ffmpeg run, so a card asks for the newest
  time once its previous frame lands and skips the times in between. Playback is the newest
  frame the tools can deliver, not the clip's full rate; a streaming decoder can replace this
  without changing the card.
- **Posters are per content hash and size bucket**, taken at the start of the clip, so the same
  clip on many cards decodes once; a smaller bucket's texture is shown while a sharper one is
  made.
- **The browser build says playback needs the desktop app** rather than hiding the card body.
