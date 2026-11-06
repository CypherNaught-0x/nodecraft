## 2026-11-06 · The browser plays video cards with its own decoder (#1069)

[`2026-10-21-video-on-cards.md`](2026-10-21-video-on-cards.md) left the browser saying playback
needs the desktop app, since cards get their frames from FFmpeg. The browser can't run FFmpeg, but
it decodes video itself.

- **Playback: the page's `<video>` element.** A playing card gets an element of its own, never
  added to the page, whose source is an object URL of the card's video. Each frame its current
  picture is drawn into a canvas no larger than the card shows (at most 1024 px on its longer
  side) and taken into the card's texture; play, pause and seek drive the element. Its sound
  plays through the page, so browser cards are heard (the desktop's are silent, #1065).
  Chosen over WebCodecs, which would need demuxing in Rust for each container and a second
  audio path, and over FFmpeg built for wasm, which is GPL with the usual builds and many
  megabytes.
- **Bounded.** At most 8 players (the least recently shown go first, freeing their URLs; cards
  shown in the same frame never push each other out, a ninth says there are too many), a video of
  at most `media`'s 256 MiB, and frames taken about 30 times a second from a canvas asked to be
  kept for frequent reads. A player whose card isn't shown is paused, so no sound plays from a
  card nobody can see.
- **Sound needs the person's click.** A page that hasn't had one may refuse to play sound; the
  card then plays muted rather than not at all.
- **What the browser can't decode says so.** Which formats play is the browser's choice (H.264
  MP4 and WebM are broadly supported). An element that fails marks the card "This browser can't
  play this video; the desktop app can" until it shows another video.
- **Rendering stays native.** The Timeline and the video actions encode with FFmpeg; in the
  browser they refuse with "… needs the desktop app", before any work. Encoding in the browser
  (WebCodecs' encoder and a muxer) is separate work.
- **Not here:** the Timeline editor's clip posters in the browser (it shows its clips without
  them), and a browser test harness. `cargo xtask wasm` checks that it builds; the repository has
  no way yet to run a card in a browser in CI, so the first browser check is by hand.
