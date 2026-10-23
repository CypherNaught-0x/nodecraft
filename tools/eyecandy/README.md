# `tools/eyecandy` — the README artwork generator

**Nothing here runs automatically.** It is not a workspace member (`crates/*`, `apps/*` and
`xtask` are the members, and `xtask` never looks at `tools/`), no build script or CI job invokes
it, and no product crate depends on it. Regenerating the gallery is a deliberate act by a
maintainer who then reviews and commits the pixels.

It exists so the pictures in [`README.md`](../../README.md) are the app rather than a drawing of
the app: each image is one frame of `NodeCraftApp::show`, rendered offscreen by the same
`egui_kittest` wgpu path that the kittest snapshot tests use (`without_display_handle`, so no
window, no display server).

## Run it

```sh
# from the repository root; writes into assets/readme/
# Use a task-owned target when sharing a checkout; no incremental cache.
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$PWD/target/agent-readme-showcase"
cargo run --offline --manifest-path tools/eyecandy/Cargo.toml -- --shots all --out assets/readme

cargo run --manifest-path tools/eyecandy/Cargo.toml -- --list           # what the shots are
cargo run --manifest-path tools/eyecandy/Cargo.toml -- --shots hero,tags # a subset
cargo run --manifest-path tools/eyecandy/Cargo.toml -- --out /tmp/art --scale 3 --max-width 3200
```

That first command, with default scale and maximum width, produced the frames in
[`assets/readme`](../../assets/readme); if you re-run it, look at every image before you commit it.
Omit `--offline` when first fetching dependencies. `--release` is optional for faster renders after
an optimised build; the documented capture used the debug binary.

| Flag | Default | Meaning |
|---|---|---|
| `--shots all` | `all` | comma-separated shot names, or `all` |
| `--out DIR` | `assets/readme` | where the PNGs go, found by walking up from the working directory so a stale binary cannot write into a different checkout |
| `--scale N` | `2.0` | pixels per point, 0.25 to 4: 2× supersampling, so text is crisp |
| `--max-width N` | `2200` | Lanczos-downscale wider frames to this; at least 320, and about twice what GitHub renders a README image at |

It needs a working wgpu adapter, and it is `egui_kittest`'s own setup that picks one
(`egui_kittest::wgpu::default_wgpu_setup`: software adapters first, then Metal or Vulkan). The tool
does not choose or configure adapters and has no fallback of its own: with no adapter at all, the
harness cannot build its renderer and the process aborts inside `render()` — so run it wherever the
kittest snapshot tests run. It never draws a substitute picture.

## What the pictures are made of

| File | Role |
|---|---|
| `src/art.rs` | Three procedural plates (dawn, aurora, bloom), painted in code: value noise, fbm, tonemap. No third-party image, no provider call. |
| `src/scene.rs` | Builds the showcase document **through engine commands only** — `node.create`, `node.add`, `node.param_set`, `node.title`, `edge.connect`, `tag.put`, `node.tag_set`, `comment.create`, `comment.reply` — plus `field.create` for persistent coloured zones and `engine::import::import_files` for the image cards, which is the same path an OS file drop takes. Ids are deterministic. |
| `src/shots.rs` | The cameras and panels, and the render pass. Frame one is spent letting the app centre itself; then the camera is set from card rects (`card::world_rect`) or an art-directed world rect. |
| `src/main.rs` | Argument parsing and the shot loop. |

Three properties are worth keeping if you edit this:

- **No invented results.** The grade, blur, aspect, noir filter, tint, mirror, two overlays and
  resize previews are the app's live action pass computing from three original imported plates.
  Dawn fans out into soft and noir paths; noir and tinted aurora recombine, then a mirrored bloom
  joins the look board. `settle_results()` waits for required results before fitting the camera,
  because previews and history strips change card bounds. `settle_previews()` then checks every
  framed image's decoded-image widget inside that node's projected card, using dimensions from
  its real asset, including imported sources and the resized result. Same-size widgets on other
  cards cannot substitute for one another. Every shot rechecks its own image widgets at capture.
  The palette gesture starts on empty canvas, never a field header (which double-click collapses),
  and the shot requires the actual focused query and expected result button before capture.
  Missing results, failed decodes and timeouts fail rather than write placeholders. No providers
  or paid models are involved.
- **Cards have a minimum zoom.** Below `card::BODY_MIN_ZOOM` the shell paints a card as an
  outline: no controls, no preview. Shots fail unless their camera exceeds that threshold.
  `check_composition` also fails if a framed card is cut by the frame edge or covered by the
  navigation overlay, counts the context cards the edge crops, and the camera pans framed
  cards clear of the overlay using `navigation::layout` before it is called.
- **The tool is a consumer of the public API.** It touches `NodeCraftApp`'s public fields and
  `pub` helpers (`theme::apply`, `CanvasView::fit`, `card::world_rect`) and nothing private, so it
  can be dropped or moved without the product noticing.

## Shot guide

All eleven original shot names remain available. `hero` and `light` now frame twelve image cards
in three coloured workflow fields, without opening a shared-parameter panel; the taller canvas
keeps real card bodies readable. `detail-grade` and `detail-chain` focus the original dawn look,
`detail-ports` the ordered text inputs, and `palette`, `tags`, `comments`, `notes` and `chrome`
show their actual app features. `modality` is an honest typed text/image routing example with
real sources, not empty video/audio/3D placeholders advertised as computed media.

Two additional closeups:

- `detail-fields`: the blue aurora field, violet screen tint, parallel noir result and their
  two-input overlay, with the cross-field dawn branch entering above.
- `detail-composite`: the pink bloom field, visibly mirrored interference plate, the second
  two-input overlay and its half-resolution review output.

Run structural tests without rendering or a GPU:

```sh
cargo fmt --manifest-path tools/eyecandy/Cargo.toml --check
cargo clippy --manifest-path tools/eyecandy/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path tools/eyecandy/Cargo.toml
```

The tests protect imported plate diversity, branch/recombination edges, field membership, honest
local-only sources, shot-name compatibility and readable hero framing. They complement, rather
than replace, visual inspection of every rendered image.

## House rules

- Keep the manifest standalone (`[workspace]` in it) and keep it out of the root workspace and
  `xtask/src/layers.rs`. It uses egui/eframe/wgpu, which only `ui-egui` and the apps may use, and
  it is not shipped code. Its standalone `[patch.crates-io]` backends must match the root
  [manifest](../../Cargo.toml): Cargo does not inherit those patches through path dependencies.
- Keep `#![forbid(unsafe_code)]` and the deny lints in `Cargo.toml`: same no-panic rules as the
  product, because a broken render should print a reason, not abort.
- After editing, `cargo clippy --manifest-path tools/eyecandy/Cargo.toml --all-targets -- -D warnings`
  should stay clean.
- Re-run all shots after any change to card, noodle, panel or theme layout, look at every image,
  and commit the ones you actually reviewed. See
  [`docs/decisions/2026-10-20-readme-artwork-is-rendered-not-screenshotted.md`](../../docs/decisions/2026-10-20-readme-artwork-is-rendered-not-screenshotted.md).
