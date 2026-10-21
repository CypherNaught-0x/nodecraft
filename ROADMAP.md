# Roadmap

**Now: the wiring milestones.** M0 is done and M1 is nearly done; the generation engine (M3) and
the action library (M4) are built and heavily tested, and the current work is connecting them to
the app. Details and exit criteria per milestone: [`docs/plan.md`](docs/plan.md) §7. Work is
tracked as GitHub issues in milestones; comment `/claim` to take one.

Status is measured from the issues in each milestone as of 2026-10-18: 271 filed, 153 open,
116 closed as completed and 2 closed as not planned. **Done** counts issues closed as *completed*,
so M1 reads 20/32 where GitHub says 21 closed (#135 was declined rather than built). Status
describes shipped behaviour, not code that merely exists: a milestone only counts what a person or
an agent can actually do today.

| M | Name | Status | Done |
|---|---|---|---|
| 0 | **Skeleton** — workspace, lints, xtask, CI, release pipeline, themed canvas | ✅ done | 10/10 |
| 1 | **Canvas core** — graph + changesets + undo, commands, canvas widget, `.ncraft` | 🟡 nearly — see caveat | 20/32 |
| 2 | **Organise** — fields, tags, comments, search, auto-format, router, input order | 🟡 partial | 6/9 |
| 3 | **Generation** — exec, queue, cost, catalog, ComfyUI, LLMs, cloud providers | 🟡 engine done, app not wired | 17/30 |
| 4 | **Actions** — image/text actions, GPU path, live recompute, custom WGSL | 🟡 library done, no action nodes | 5/12 |
| 5 | **Batch & Techniques** — batch, Technique Builder, app mode, library, CLI `run`, release | 🟡 batch planning engine (#92), app mode (#95), installing updates (#540), result export (#557), CLI `run` (#97) | 2/16 |
| 6 | **Media editors** — Layer Editor, Mask/Inpaint, Elements | 🟡 mask editor (#102), Layer Editor (#101), Elements (#104) | 3/6 |
| 7 | **Video & audio** — nodes, playback, Timeline, FFmpeg, audio/video providers | 🟡 sidecar, playback on cards (#105), video generation (#559), Timeline (#109) | 4/15 |
| 8 | **3D & documents** — 3D viewer, capture, GLB/OBJ, PDF, 3D providers | 🟡 3D node and viewer (#111), 3D photo and view sheet (#112), Tripo 3D generation (#22), PDF node (#113), Meshy 3D generation (#23) | 5/7 |
| 9 | **Agent & automation** — MCP, control channel, agent sidebar, serve | 🟡 surfaces done, agent sidebar (#114), HTTP serve (#117, polling) | 2/6 |
| 10 | **Share & collaborate** — browser polish, CRDT sync, presence, decks | ⬜ not started | 0/6 |
| — | **Future providers** — the 25 further adapters tracked as `future` | ⬜ none started | 0/26 |

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
  The viewer's Photo and Views buttons add a 3D photo or a
  4/8/16-view sheet card that draws the model from the current view as an image (#112). PDFs become PDF cards showing the
  first page and page count; PDF to images draws a page range as images and PDF to text reads
  the text the pages draw (#113). A PDF reader pages through a PDF, sets a node's page range, and extracts a page or
  its text into new nodes (#590).
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
  project as evidence of what was billed; it is never turned into money (#437).
- **Document integrity**: every edit is an invertible changeset; undo/redo with a bounded stack;
  command dispatch under a panic guard; 66 registered commands with typed JSON params.
- **Automation**: `nodecraft-cli mcp` (MCP over stdio, 4 generic tools), `mcp --bridge` to drive a
  running app, `nodecraft-cli control`, and `nodecraft --control` (local JSON control channel with
  screenshots). See [`docs/mcp.md`](docs/mcp.md) and [`docs/control.md`](docs/control.md).
- **Headless**: the planner, scheduler, driver, run executor, cost quotes, budgets, usage ledger,
  staleness hashing and router resolution exist and are tested end to end against the mock
  provider.

## What does not work yet

The honest list, because "it's in the repo" and "you can use it" are different claims:

- **The app cannot generate.** Image generation cards (`generation.image`, #465) can be placed,
  configured and swapped between catalog models (#157), but nothing constructs a provider adapter
  or installs the run host, so `run.start` reports *unsupported* (#409, #362). Five adapters (LLM
  endpoints, OpenAI Images, Replicate, ComfyUI and Seedream) have tested library integrations,
  with catalog definitions available to engine callers.
- **Actions are partly live.** The 13 image and 4 text operations are node kinds (#410) and
  recompute on a worker after each edit, joining that edit's undo step (#433). Reduced-resolution
  previews while a slider drags, and recompute in the browser build, are still open (#88).
- **The in-canvas agent works in the desktop app.** Cmd+/ opens the agent sidebar (#114): ask in
  words, with the selection and @-mentioned nodes as context. `crates/agent` runs the model's tool
  calls over the MCP tools, judging each one first (`Session::impact`): assist mode asks before any
  change or run, auto mode runs edits and runs within the `[agent]` budget. The endpoint (Anthropic,
  OpenAI, OpenRouter, Ollama, LM Studio) and model are set in `[agent]` in the settings file; there
  is no settings screen for them yet, and the browser build has no agent.
- **The browser build is a bare preview.** `apps/nodecraft-web` runs the canvas in a browser and
  releases ship it as `nodecraft-web-<v>.zip` (#99), but no browser host provides project storage,
  keys or runs yet. Files dropped in a browser are read asynchronously and added as nodes (#319);
  pasting a copied image works in the desktop app only.
- **Video needs your own FFmpeg.** The desktop app plays videos on cards (#105) with the ffmpeg
  and ffprobe set under `[media]` in settings; it neither finds them on `PATH` nor bundles them
  (#411). There is no audio playback (#106) and no video actions (#108).
  Video generation (#559) has text, frame, reference and speech-to-video modes declared per
  model; fal's Wan 2.2 maps text, first/last-frame and speech to video (#607), and no adapter
  maps reference-to-video yet.
  The Timeline (#109) renders clips with fades and cross-fades on a main track, with picture
  tracks over it and sound tracks mixed under it (#576).
- **Not started**: fields UI (the data model and changes exist, but there is no fields editor),
  batch, Techniques, app mode, library, Layer editor, Elements, PDF,
  collaboration, and 33 of the 41 tracked provider adapters (ComfyUI, fal.ai and
  Gemini are in progress; Comfy Cloud, Google Veo and TTS, Seedance, Kling, ElevenLabs,
  and the 25 `future` names have no work started). 3D generation has a Generate 3D card
  with Tripo text, image and multiview models and fal's Trellis (#22), and Meshy text, image,
  multi-image, retexture and remesh models (#23).

## Next up

The open pull requests say what is in flight; the near-term order that makes the engine usable is:

1. ~~**Wire the app to storage**~~ — done: session storage (#406), open/save dialogs (#407) and
   the recovery prompt (#408), so save, open and crash recovery work in `cargo run -p nodecraft`.
2. **Wire a run host** — construct adapters from the catalog (#409), register the `run.start`
   service and the host resources (#362), and put a Run button on the card (#211, #201, #157).
3. **First generation node kinds** — image/text generation kinds so there is something to run
   (#83, #84), then stale badges with one-click refresh (#211).
4. **Action nodes** — expose the CPU/GPU operations as node kinds (#410) with live recompute
   (#88, #90).
5. **Organise** — fields UI (#66). Tags (#67), comments/Notes (#68), selected-node auto-format
   (#70), the bulk parameter panel (#71), and the v3 tags/fields persistence bridge (#193) have shipped.
6. **Then M5**: batch, Technique Builder, app mode, library, and the CLI `run`/`quote`/`inspect`
   that turn a canvas into a reusable command, and the first signed release (#100).

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
