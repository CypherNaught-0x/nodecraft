# Decisions

Decisions others will build on, newest first. The product-level ones are in
[`plan.md`](plan.md) §6; this log records the ones taken while building. Add an entry when you make
a choice a later contributor might otherwise undo.

## 2026-10-17 · ComfyUI protocol layer (#9, part 1)

- **`comfy` is sans-IO.** It builds requests (`Server`) and parses replies and WebSocket
  messages; `providers` owns the transport (reqwest on desktop and wasm, a WebSocket client per
  platform). No async runtime in L1, and every protocol path is tested from fixtures.
- **Clean-room:** written from ComfyUI's public HTTP/WebSocket protocol and JSON formats. ComfyUI
  is GPL-3.0, so its source is never read or copied; fixtures are hand-written.
- **Input order comes from `/object_info`** (`input_order` when present, else document order via
  a small ordered JSON type in `comfy`). The workspace does not enable serde_json's
  `preserve_order`, which would change map ordering, and so hashing, in other crates.
- **UI workflows are converted against a live server's catalog**: widget values are positional.
  Seed "control after generate" values are kept as modes and the upload-button value is skipped;
  reroutes, primitives, Set/Get variable nodes and bypassed nodes (slot by slot) are followed
  (bounded, cycle-safe); notes and muted nodes are dropped. Widget counts that don't line up and
  unknown unconnected nodes become warnings, not silent guesses. Subgraphs and group nodes are
  refused with a clear message for now.
- **Missing custom nodes and models are found before queueing** (`ApiWorkflow::check`), so the
  node shows an error card instead of a server error.

## 2026-10-16 · Settings through craft-settings (#55)

- **App id `nodecraft`** for the folder and `NODECRAFT_CONFIG_DIR`; schema version 1.
- **Portable marker is `portable.txt`** (the Windows portable zip ships one), so the zip tells
  people what the file does; settings go in `config/` beside the executable.
- **Values from the file are hostile:** read them through clamping accessors
  (`CanvasSettings::zoom_speed()`), and an unknown enum value resets only its section.
- **A file with a newer `schema_version` is read but never overwritten** (craft-settings makes
  the store read-only), so running an older build doesn't destroy a newer build's settings.
- **The store lives in the shell** (`NodeCraftApp` ticks it each frame and flushes on exit) and
  is native only; the browser build uses defaults. Changing settings from the UI, CLI or MCP waits
  for the command registry (#52) and goes through a `settings.set` command.
## 2026-10-16 · media: sniffing, decode, thumbnails (#56)

- **Formats are sniffed from bytes, never from file names**, and the sniffed MIME type picks the
  decoder (`ImageReader::set_format`), so a mislabelled file can't reach a different decoder.
- **`image` without default features**, decoding PNG, JPEG, GIF, WebP, BMP and TIFF. No `rayon`
  (wasm) and no AVIF/HEIC decode yet: those are sniffed and reported as unsupported. Animated GIF
  and WebP decode to their first frame.
- **Every decode is capped** (`media::Limits`: 256 MiB input, 16384 px per side, 1 GiB decoder
  allocations by default). The caps are checked from the header before pixels are allocated:
  most `image` decoders ignore `max_alloc`, so `media` reserves the decoded size (plus the copy an
  EXIF rotation makes) against it itself.
- **Images are upright:** EXIF orientation is applied on decode, and `ImageMeta` reports the
  displayed size.
- **`media::ContentHash` is BLAKE3**, matching the asset store's `assets/<blake3>` names. `media`
  can't depend on `types`, so `types`' hash type should convert from its `[u8; 32]`.
- **Thumbnails are RGBA8 with straight alpha**, cached by (content hash, max edge) in a
  memory-budgeted LRU (`ThumbnailCache`). The cache isn't thread-safe; its owner decides where
  thumbnails are made.

## 2026-10-16 · Engine command sessions (#52)

- **Handlers return edits against immutable state.** They perform no I/O; the session applies
  changesets to a draft, validates the undo/redo budget, then publishes the document, history
  and revision event together. Native unwinding is caught before publication. Abort-only wasm
  builds still depend on fallible input validation rather than panic recovery.
- **Drag coalescing is explicit.** A frontend supplies a token unique to one gesture. Only
  consecutive moves of the same node on the same canvas with that token coalesce. Undo/redo,
  another edit, a different token or an omitted token starts a new step. The first inverse
  restores the pre-drag position; redo uses the final position.
- **Undo and event history are bounded.** Each undo/redo stack retains at most 256 steps and
  16 MiB of serialized changesets. New edits must fit both their inverse and actual redo
  representation. Each consumer holds a revision cursor into the last 256 `DocChanged`
  notifications; an expired cursor explicitly requires a document resync.

## 2026-10-16 · Graph transactions (#51)

- **Stored document maps are private.** Detached node data is editable; a stored document changes
  only through serializable changesets. Deserializing a project runs structural validation.
- **Changesets are ordered and atomic.** Existence, payload and count checks apply per edit;
  relationships are checked once at commit. Temporary cycles, occupied input orders or missing
  field/tag references can be repaired within the batch. New edges require existing nodes,
  and carried snapshots must pass payload validation. Staging one clone keeps failed edits from
  partially mutating the live document. Inverses use the same bounded path as forward edits.
- **History edits are granular.** Indexed generation insertion/removal and active selection
  preserve history order without serializing the whole node in every undo record.
- **Aggregate JSON budgets cover both directions.** Nodes are capped at 8 MiB, changesets and
  their generated inverses at 64 MiB, and projects at 256 MiB. Reject an oversized undo before
  publishing its forward edit. These limits and graph/journal variants belong to the file
  version contract; check that version before deserializing and overwriting a project.
- **Connections form a DAG.** Ports must have matching modalities, a list output cannot feed a
  single input, and each input order is unique. Cycles are rejected with an iterative walk.
- **Node deletion has a compact inverse.** One restore operation reinstates the node and its
  incident edges, avoiding an undo operation-count overflow for highly connected nodes.

## 2026-10-16 · Core types (#50)

- **Entity IDs are typed UUIDs supplied by the caller.** The pure-data layer has no random
  number generator or platform dependency. Serialized UUID strings also work as JSON map keys.
- **Money uses integer millionths plus a currency code.** Arithmetic is checked and rejects
  mixed currencies. The amount is a decimal string on the wire, avoiding loss in browser JSON
  clients above the exact-integer range of floating-point numbers.
- **Fractional prices are aggregate rates, rounded once after usage multiplication.** Keep a
  $0.075-per-million-token price as 75,000 micros per million, then use `checked_mul_ratio` with
  total usage and a denominator of one million. Its u128 intermediate avoids premature
  rounding and overflow. Budget quotes round up, so a non-zero fractional cost cannot bypass
  a budget; settled amounts may explicitly round down where the provider contract specifies it.
- **Unknown error categories preserve their message as `ErrorKind::Unknown`.** Unknown
  `Modality` and `Value` variants deliberately fail decoding: new variants require a `.ncraft`
  format-version gate before graph deserialization. The format layer must reject unsupported
  versions with an actionable update/compatibility error, never silently discard unknown data.
- **MIME essence selects decoders.** `Mime::essence()` compares normalized type/subtype;
  full equality retains parameter spelling and spacing rather than promising semantic equality.
- **Asset hashes and MIME values validate on deserialization.** Media metadata uses optional
  integers; resource limits belong to the decoder consuming those measurements. `Value` keeps
  nested lists, so project readers must retain their decoder recursion and input-size limits.

## 2026-10-16 · M0 skeleton

- **Layer numbering follows the plan (L0–L5), not PhotoCraft's L0–L7.** The rule "only the shell
  uses UI crates" is an allow-list (`ui-egui`), because `agent` and `automation` share the shell's
  layer but must stay headless.
- **`media` is standalone** (no workspace deps). `graph` depends on it, not the other way round.
- **Empty crates already declare their planned workspace dependencies**, so the layering check is
  meaningful from day one and the dependency graph is visible in `cargo xtask layers`.
- **craft-libs pinned at `8f5a283`.** `craft-ui` and `craft-quit` are used by the shell now;
  `craft-settings`, `craft-atomic` and `craft-pdf` are pinned in `[workspace.dependencies]` but
  unused until M1/M8. Note that `craft-pdf` relies on `[patch.crates-io]` entries in craft-libs'
  own workspace (vendored `krilla`/`hayro`), which do not apply to a git dependency; check this
  when `craft-pdf` is first used.
- **`ui-egui` depends on eframe with `wgpu`** (not only egui) so the `eframe::App` impl lives in
  the shell and `cargo test -p nodecraft-ui-egui` builds on its own; eframe without a renderer
  feature does not compile on macOS.
- **Canvas navigation:** scroll pans and pinch/⌘-scroll zooms (trackpad-first, like the reference
  products); see `ui-design.md`.
- **Releases:** our own `release.yml`, modelled on the shared Craft release workflow, because the
  shared one fails on the `release` branch without Apple and Windows signing secrets and we have
  none yet (#100). Ours signs when secrets exist and otherwise ships unsigned builds with
  first-launch instructions (`docs/install.md`). It is written from scratch (craft-common-actions
  has no licence file, so its text isn't copied). Targets: macOS universal, Windows x64/arm64,
  Linux x86_64/aarch64 + Flatpak; the web job comes with `apps/nodecraft-web` (#99). Switch back
  to the shared workflow once signing is in place, if it is still the better fit.
