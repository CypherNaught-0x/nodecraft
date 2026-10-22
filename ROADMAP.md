# Roadmap

**Now: M1 canvas core is complete; the later milestones are in progress.** The desktop can
edit, save and run projects, while milestone exit criteria for generation, reuse and sharing still
have open work. Details and exit criteria: [`docs/plan.md`](docs/plan.md) §7. Work is tracked as
GitHub issues in milestones; comment `/claim` to take one.

As of 2026-10-22, **Done** counts only issues closed as *completed* (not `not planned`); the
denominator is all filed issues, including open and declined issues. M1 is 35 completed, one
`not planned` (#135), and this close-out (#372) still open: its feature work and #65's exit
criterion are done, but GitHub will only close the milestone after this issue lands. Other rows
likewise reflect GitHub's live issue state, not a declaration that all milestone exit criteria
have passed. Status distinguishes tested library capabilities from what someone can use in the app.

| M | Name | Status | Done |
|---|---|---|---|
| 0 | **Skeleton** — workspace, CI, release pipeline, themed canvas | ✅ feature work done | 10/10 |
| 1 | **Canvas core** — graph, undo, commands, canvas, `.ncraft` | ✅ feature work done; close-out open | 35/37 |
| 2 | **Organise** — fields, tags, comments, search, format, router | 🟡 partial | 10/14 |
| 3 | **Generation** — exec, queue, cost, catalog, providers | 🟡 partial; desktop run host wired | 38/44 |
| 4 | **Actions** — image/text actions, GPU, live recompute | 🟡 partial; action nodes work in app | 8/12 |
| 5 | **Batch & Techniques** — batch, app mode, library, CLI, release | 🟡 partial; first signed release pending | 16/27 |
| 6 | **Media editors** — Layer Editor, Mask/Inpaint, Elements | 🟡 partial | 6/10 |
| 7 | **Video & audio** — playback, Timeline, FFmpeg, providers | 🟡 partial; Generate audio card (#672), ComfyUI video/audio (#110) | 16/30 |
| 8 | **3D & documents** — viewer, capture, PDF, providers | 🟡 partial | 6/12 |
| 9 | **Agent & automation** — MCP, control, agent, serve | 🟡 partial; CLI serve exists, #117 open | 4/8 |
| 10 | **Share & collaborate** — browser, sync, decks | 🟡 partial; presence pending | 6/14 |
| — | **Future providers** — separately tracked adapters | 🟡 partial | 1/27 |

## What works today

Run it: `cargo run -p nodecraft`. What the window actually does:

- **Canvas**: pan/zoom, dot grid, graph-backed node cards whose controls are generated from the
  node's parameter schema, noodles (draw, rewire, delete), selection by click/Shift/marquee and
  upstream/downstream/kind, minimap with zoom-to-fit and zoom-to-selection (Shift+1/Shift+2/Shift+M), incremental
  search (⌘F), double-click quick-add palette with per-key shortcuts, "Turn into" on
  drop-to-empty, input-order badges with drag-to-reorder, copy/cut/paste/duplicate through the OS
  clipboard, OS file drop as source nodes, split-grid image previews (#337), per-node generation
  history strip, local pinned
  discussions with Note nodes (#68), grid/dependency formatting of selected nodes (#70), named
  colour tags with a bounded highlight filter (#67), shared parameter editing with explicit Mixed
  values and atomic undo across compatible selected nodes (#71), and an API keys panel backed by
  the OS keychain. Shared model changes show per-target migration results before applying (#157).
  A mask editor paints brush masks over an image, expands its canvas and wires an inpaint or
  outpaint generation card from a prompt (#102).
  A Layer Editor card composites its wired images with text and shape layers into one frame;
  its full-screen editor moves, resizes, rotates, snaps and aligns layers (#101). Elements turn
  selected images into a named, reusable subject that prompts mention as `#name`, with a
  generated reference sheet (#104). 3D models (glTF, GLB, OBJ) become 3D nodes whose cards show
  the model in an orbit viewer drawn with wgpu, shaded, unlit or as a wireframe; Convert 3D
  writes a GLB or OBJ copy (#111). Typing `@` in a generation prompt mentions another card,
  whose text goes into the prompt where it is mentioned and which runs first, like a wire (#505).
  Present selection makes a Deck card with a slide per card and presents it full-window; slides
  are layer stacks whose source layers show what their card shows now, and `deck.*` commands
  list and edit them with revision checks (#122). Edit slides opens a slide sorter beside the
  Layer Editor's stage, to arrange each slide's text, shapes and sources (#694).
  The viewer's Photo and Views buttons add a 3D photo or a
  4/8/16-view sheet card that draws the model from the current view as an image (#112). PDFs become PDF cards showing the
  first page and page count; PDF to images draws a page range as images and PDF to text reads
  the text the pages draw (#113). A PDF reader pages through a PDF, sets a node's page range, and extracts a page or
  its text into new nodes (#590). Scanned pages are recognised by a vision-capable text model: PDF to
  text headings mark pages that draw no text, PDF to images can draw only those, and the built-in
  chat models take up to 16 images on a text card (#592).
  Improve prompt on the generation toolbar puts a text card, which rewrites the card's typed
  prompt, in front of it and wires the rewrite in; it runs, and is priced, like any other card (#582).
  The builtin catalog lists chat models on the OpenAI and Anthropic endpoints (GPT-6.1 Sol,
  GPT-6 Luna, GPT-4.1 mini; Claude Opus, Sonnet and Haiku 5.5), quoted up to their max output
  tokens, so a Generate text card has models to pick (#503).
  A paid result that doesn't fit its card or can't be recorded is held in the queue panel,
  to retry, put on the canvas as source nodes, or discard, without paying again (#369), and so
  are a fanned step's results, as one (#611). The desktop saves held results so they survive a
  restart (#614).
  A fal result's `X-Fal-Billable-Units` header is kept, exactly, with the job's settings in the
  project as evidence of what was billed; it is never turned into money (#437). Run quotes also
  surface aggregate admission feedback before confirmation (#307). Imported video and audio
  sources probe duration (#608); ComfyUI accepts video and audio workflows (#110). Plugin cards
  keep their package identity, migrate with their package, and survive uninstall (#173).
- **Document integrity**: every edit is an invertible changeset; undo/redo with a bounded stack;
  command dispatch under a panic guard; a shared registry of stable dotted commands with typed JSON params.
- **Automation**: `nodecraft-cli mcp` (MCP over stdio, 4 generic tools), `mcp --bridge` to drive a
  running app, `nodecraft-cli control`, and `nodecraft --control` (local JSON control channel with
  screenshots), plus `nodecraft-cli serve` for a project's published Techniques over a local
  authenticated HTTP API with optional signed webhooks. See [`docs/mcp.md`](docs/mcp.md),
  [`docs/control.md`](docs/control.md) and [`docs/serve.md`](docs/serve.md).
- **Headless**: the planner, scheduler, driver, run executor, cost quotes, budgets, usage ledger,
  staleness hashing and router resolution exist and are tested end to end against the mock
  provider.

## What does not work yet

The honest list, because "it's in the repo" and "you can use it" are different claims:

- **Generation is not finished across every model or surface.** The desktop run host now constructs
  supported catalog adapters and runs authorized generation cards (#409, #362, #83); a listed model
  is not proof that every provider, modality or input mode runs in every host. The text card's
  chat-style turn UI remains open (#84), and browser provider settings/option lists differ from
  desktop (#721). Check [`docs/providers.md`](docs/providers.md) for adapter-specific limits.
- **Local actions work in the app.** Image/text action node kinds (#410) recompute on a worker
  after edits (#88); slider drags preview with smaller images before full-size rendering. The
  browser has no equivalent worker threads and runs rounds in the frame. This is not evidence
  that all paid provider operations or video/audio actions work in every host.
- **The in-canvas agent works in the desktop app.** Cmd+/ opens the agent sidebar (#114): ask in
  words, with the selection and @-mentioned nodes as context. `crates/agent` runs the model's tool
  calls over the MCP tools, judging each one first (`Session::impact`): assist mode asks before any
  change or run, auto mode runs edits and runs within the `[agent]` budget. The endpoint (Anthropic,
  OpenAI, OpenRouter, Ollama, LM Studio) and model are set in `[agent]` in the settings file; there
  is no settings screen for them yet, and the browser build has no agent.
- **The browser build is a bare preview.** `apps/nodecraft-web` runs the canvas in a browser and
  releases ship it as `nodecraft-web-<v>.zip` (#99). Provider keys are kept in the browser's
  storage for the site, and projects open and save through the browser's file pickers or as
  downloads (#118). Cards run in the browser too, calling the cloud providers that accept
  requests from web pages directly with those keys (#118). ComfyUI and LiteLLM endpoints and
  option lists are desktop only for now (#721), and so is video. Unsaved work is kept in the
  site's storage and offered back after a crash or a closed tab (#717); leaving with unsaved work
  still asks first. Files dropped in a browser are read asynchronously and added as nodes (#319);
  pasting a copied image works in the desktop app only.
- **Video needs your own FFmpeg.** The desktop app plays videos on cards (#105) with the ffmpeg
  and ffprobe set under `[media]` in settings; it neither finds them on `PATH` nor bundles them
  (#411). Sound cards show a waveform and play through the default output device (#106); there
  is no sound while a video plays on its card yet. Video actions (#108) trim, change speed, stitch, replace
  sound, extract frames, reverse, grade, key, watermark, split and extract sound.
  Video generation (#559) has text, frame, reference and speech-to-video modes declared per
  model; fal's Wan 2.2 maps text, first/last-frame and speech to video (#607), and no adapter
  maps reference-to-video yet. Gemini Omni Flash video (#17) supports text, first/last-frame and
  reference-image inputs; Veo on Google Cloud is still tracked separately (#742). A Generate
  audio card (#672) runs the catalog's text-to-audio and audio-to-audio models, such as fal's
  Kokoro speech, and plays its result.
  The Timeline (#109) renders clips with fades and cross-fades on a main track, with picture
  tracks over it and sound tracks mixed under it (#576), and captions over the picture (#574);
  each picture clip can be placed, sized, cropped, turned and faded in the frame, clips can play
  faster or slower, and the render has a quality (#577).
- **Live sharing works from the desktop app.** Sessions are shared through `nodecraft-cli relay`
  (#119), with every edit merged and images passed along. Share links let someone view, comment
  on or edit a shared project, with signed discussions and expiry and revocation (`share.link_*`,
  #121), and canvases kept private (`canvas.set_private`) stay out of the room. The app's Share
  chip shares a project and shows its join link on request; Join opens a shared project in a new
  window, and a dropped connection reconnects on its own (`share.start`, `share.join`,
  `share.stop`, `share.status`, #698). The relay is reached over `ws://` only, the browser build
  has no transport yet, and there is no presence.
- **Remaining gaps are specific, not whole missing milestones.** Batch, Techniques, app mode,
  the Layer Editor, Elements and PDF cards have shipped in some form. The first signed release
  (#100), typed nested descriptor records/lists (#528), and browser option-list parity (#721)
  remain open. HTTP serve's tracker #117 is open although `nodecraft-cli serve` and signed
  completion webhooks are already in current source; see [`docs/serve.md`](docs/serve.md).
  Provider availability and modes vary by adapter and host: Google Gemini Omni Flash video
  replaces the shut-down Veo API (#17), while Veo on Google Cloud (#742), Google TTS, Seedance,
  Kling and most `future` adapters remain tracked separately. ElevenLabs speech (v3,
  Multilingual v2, Flash v2.5), effects, music and voice changer models (#20) have a Generate
  audio card (#672); 3D has Tripo text/image/multiview and fal Trellis (#22), plus Meshy
  text/image/multi-image/retexture/remesh (#23). Consult [`docs/providers.md`](docs/providers.md)
  rather than inferring app support from a catalog entry. Published pre-releases are snapshots,
  not the current `main`.

## Next up

The near-term order follows the remaining open issues, not an already-finished wiring checklist:

1. **Close M1 paperwork** (#372): save/open/recovery, the run host and the canvas performance
   exit criterion (#65) have landed. On Apple M3 Pro at 120 Hz, the desktop presented-frame
   pan/zoom run met P1–P3 for 200 and 1,000 nodes (#634); this does not assert a result on 60 Hz,
   Windows or Linux hardware. See [`docs/performance.md`](docs/performance.md).
2. **Finish current M2/M3 interactions**: the chat-style text-card turn UI (#84) and remaining
   open milestone issues; do not mistake a tested adapter for a working mode in every host.
3. **Finish reusable delivery**: complete the remaining M5 work and first signed release (#100),
   then browser option-list parity (#721), verification/closure of HTTP serve's still-open
   tracker (#117), and later milestone gaps.
   For the changing priority order, use the [open milestone issues](https://github.com/CypherNaught-0x/nodecraft/milestones).

## Milestone log

- **2026-10-16** — M0 closed: workspace, lints, `xtask layers`/`wasm`, CI, docs, themed canvas,
  release pipeline. `v0.1.0-rc.1` was built and published as a pre-release with installers for
  macOS, Windows and Linux.
- **2026-10-16 → 18** — M1 pulled almost in: ids and values, media sniffing/decode/thumbnails,
  node kinds and schemas, the document model with invertible changesets, the command registry with
  undo and an event bus, rendered editable cards, noodles, selection, minimap, quick-add,
  copy/paste/duplicate, "Turn into", input ordering, file drop, search, and `.ncraft` save/load
  with archives, atomic saves and crash recovery (library side).
- **2026-10-17 → 18** — M3 engine and M4 library built ahead of their milestones: catalog as data,
  planner, scheduler, driver, run executor, cost quotes, budgets held while jobs run, usage ledger,
  staleness hashing, submission certainty and delivery evidence, keychain-backed keys with a keys
  panel, testkit mock provider and golden graphs, 13 CPU image actions plus 4 text actions with a
  bounded GPU path, and the LLM/OpenAI Images/Replicate/ComfyUI adapters. Also MCP over stdio and
  its bridge, the control channel over local JSON and stdio, split-into-grid, and the FFmpeg
  sidecar.
- **2026-10-18** — docs brought in line with the code. `docs/decisions.md` was split into one
  dated file per decision under `docs/decisions/` (#295, #303), with `cargo xtask decisions`
  enforcing the format so parallel pull requests never conflict over one shared log.
- **2026-10-18** — `v0.2.0-rc.1` published: the first build cut from the post-M0 tree, with
  installers for macOS and Windows and packages plus Flatpaks for Linux. A pre-release on
  purpose — the app still cannot generate or save; the 0.2.0 GA belongs with the M3 wiring above.
- **2026-10-18** — ticket audit against this file: six missing tickets filed for work the roadmap
  named but nothing owned — desktop storage (#406), file picker (#407), recovery prompt (#408),
  catalog-to-adapter construction (#409), action node kinds (#410) and shipping the FFmpeg
  sidecar (#411). Cards now render image previews (#386) and discussions shipped (#68), so stats,
  milestone counts and command counts (now 57) are refreshed.

- **2026-10-18** — selected-node auto-format shipped (#70): grid and dependency ordering use
  actual card geometry and one undo step. Comments/Notes (#68) remain available alongside the
  Format control; the command registry now has 58 entries.

- **2026-10-18** — colour tagging and its bounded highlight filter shipped (#67), backed by
  the v3 tags/fields persistence bridge (#193). Bulk assignments undo atomically, and legacy
  archives/recovery migrate before new appends. The command registry now has 66 entries.

- **2026-10-19** — the bulk parameter panel shipped (#71): compatible parameter intersections,
  explicit Mixed values, conditional resets and one atomic undo step. Shared model changes reuse
  the migration preview (#157), including dropped settings and unavailable-model repair. File
  operations preserve pending bulk drafts and own input through their completion frame.

- **2026-10-22** — fields shipped (#66): named, coloured zones under the cards. `F` makes one
  around the selection (or an empty one at the pointer), a header drag moves it with every
  member, the corner resizes it, and collapsing hides the members. Five commands: `field.create`,
  `field.update`, `field.move`, `field.remove` and `node.field_set`.
- **2026-10-22** — M1 feature work and its exit criterion finished: #549 removed the quadratic
  after-edit live-action plan (measured 2.28/7.84 ms at 200/1,000 nodes on a busy Apple M3 Pro;
  not comparable to the quiet #203 baseline). #65 closed after #634 measured presented pan/zoom
  frames on a 120 Hz Metal display against P1–P3. This closes the 2026-10-20 17.09/283.55 ms
  CPU regression report, **not** a blanket 60 fps claim across hardware or all edit workloads.
  GitHub still has #372 open until this documentation close-out lands.
