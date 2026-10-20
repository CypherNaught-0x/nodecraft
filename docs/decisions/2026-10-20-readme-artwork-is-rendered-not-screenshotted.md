## 2026-10-20 · README artwork is rendered, not screenshotted (#495)

- **The rule stands, with one named directory excepted.** AGENTS.md says attach media to the PR and
  never commit generated screenshots, and `dev_assets/` stays study-only. `assets/readme/` is the one
  place where committed rendered frames are intended: a README gallery is documentation that has to
  survive the PR that introduced it, and a hosted `user-images` URL goes stale. Nothing in the
  product, the release job or CI reads that directory, so the exception has no reach beyond the
  front page.
- **The frames are the app, not a picture of the app.** `tools/eyecandy` renders one frame of
  `NodeCraftApp::show` through `egui_kittest`'s headless wgpu (`without_display_handle`), the same
  path the kittest snapshot tests use. Scenes are built only with engine commands — `node.create`,
  `node.add`, `node.param_set`, `node.title`, `edge.connect`, `tag.put`, `node.tag_set`, `comment.create`, `comment.reply` — the
  plate enters through `engine::import::import_files` (the OS file-drop path), and the graded,
  softened and cropped previews in the pictures are the live action pass (#88) recomputing on the
  app's own worker. The app is never asked to pretend: what a card shows is what it had computed.
- **Opt-in by construction.** `tools/eyecandy` is a standalone workspace, not a root member, not in
  `xtask/src/layers.rs`, depended on by nobody and invoked by nothing. `cargo xtask ci` and
  `cargo xtask wasm` neither build nor run it. That keeps egui/wgpu out of the layering rules for
  shipped code and keeps a broken GPU from being able to break a build.
- **Two app behaviours the tool must respect**, worth knowing before editing a shot: a card below
  `card::BODY_MIN_ZOOM` (55%) is painted as an outline with no controls and no preview, so a
  too-wide camera silently produces a picture of empty boxes; and the first frame centres the
  document at 100%, so a camera set before it is overwritten. `shots.rs` checks both and says so on
  stderr rather than shipping a dead-looking image.
- **PNGs are the artifact, the tool is the provenance.** Output is 2× supersampled and
  Lanczos-downscaled to at most 2200 px wide (the default `--max-width`), and it is *not* byte-reproducible across GPUs, font
  builds and text-hinter versions. A re-run can therefore produce a diff nobody typed. Review the
  images you commit the way you review a screenshot, and say in the commit how they were made
  (`tools/eyecandy/README.md` has the exact command).
- **Attribution.** The three procedural plates in `art.rs` and the PNGs they produced are original
  NodeCraft work under CC0-1.0; the generator's Rust code is under the repository's MIT OR
  Apache-2.0, as its manifest declares. Both rows are in `ATTRIBUTION.md`. No third-party screenshot,
  mock-up or reference image was used or adapted.
