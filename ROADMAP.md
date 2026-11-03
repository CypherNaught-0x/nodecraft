# Roadmap

The table tracks completed issues and remaining work across NodeCraft's milestones. See
[the plan](docs/plan.md#7-roadmap) for exit criteria and
[GitHub milestones](https://github.com/CypherNaught-0x/nodecraft/milestones) for current work.

**Done** counts issues closed as completed. The denominator includes open and declined issues.
Counts use the GitHub snapshot taken for these close-outs and include close-outs marked complete below.
M1 and Future providers each have one declined issue (#135 and #28).

| M | Name | Status | Done |
|---|---|---|---|
| 0 | **Skeleton** — workspace, CI, release pipeline, themed canvas | ✅ done | 10/10 |
| 1 | **Canvas core** — graph, undo, commands, canvas, `.ncraft` | ✅ done | 36/37 |
| 2 | **Organise** — fields, tags, comments, search, format, router | ✅ done | 14/14 |
| 3 | **Generation** — exec, queue, cost, catalog, providers | 🟡 partial with template mappings (#948) and close-out (#374) open | 47/49 |
| 4 | **Actions** — image/text actions, GPU, live recompute | ✅ done | 14/14 |
| 5 | **Batch & Techniques** — batch, app mode, library, CLI, release | 🟡 partial with the Batch node complete (#92) and signed release pending (#100) | 28/34 |
| 6 | **Media editors** — Layer Editor, Mask/Inpaint, Elements | 🟡 partial with provider-trained references (#180), SOUL 2 scene inputs (#1043) and close-out (#377) open | 11/14 |
| 7 | **Video & audio** — playback, Timeline, FFmpeg, providers | ✅ done with close-out #378 | 31/31 |
| 8 | **3D & documents** — viewer, capture, PDF, providers | 🟡 partial | 10/16 |
| 9 | **Agent & automation** — MCP, control, agent, serve | 🟡 HTTP serve with signed webhooks (#117), with close-out (#380) open | 7/8 |
| 10 | **Share & collaborate** — browser, sync, decks | ✅ done with close-out #381 | 17/17 |
| — | **Future providers** — separately tracked adapters | 🟡 partial | 26/29 |

ComfyUI workflow cards expose prompts and common generation settings on import. Their
**Edit workflow fields…** dialog saves custom field visibility, names and order without
reimporting, including retained image previews for compatible core workflows (#947).
The model picker's ComfyUI group lists the default server's own models by type once the server
is read. Checkpoints run through the built-in FLUX or SDXL workflow. Models with no built-in
workflow are shown as unavailable, with the reason, and imported workflow cards are listed by
title (#970).

Runway Gen-4.5 text/first-frame video and Gen-4 Turbo first-frame video are available through
priced catalog models and the direct task adapter (#25); paid live verification remains opt-in.

Official workflow starters are maintained in the separate
[nodecraft-templates](https://github.com/CypherNaught-0x/nodecraft-templates) repository. Native and
browser builds embed a pinned offline set; the Techniques panel can refresh the versioned index
and retrieve one verified version without importing or running it (#953). **Update templates**
(`templates.update`) downloads new and changed templates in one bounded, explicit update, each
checked against its index digest, into a versioned cache in the app data folder that is verified
again at startup and can be cleared (#954).

## What works today

Run it: `cargo run -p nodecraft`. What the window actually does:

- **Canvas**: pan/zoom, dot grid, graph-backed node cards whose controls are generated from the
  node's parameter schema, noodles (draw, rewire, delete), selection by click/Shift/marquee and
  upstream/downstream/kind, minimap with zoom-to-fit and zoom-to-selection (Shift+1/Shift+2/Shift+M), incremental
  search (⌘F), double-click quick-add palette with per-key shortcuts, "Turn into" on
  drop-to-empty, input-order badges with drag-to-reorder, copy/cut/paste/duplicate through the OS
  clipboard, OS file drop as source nodes, OS folders (dropped, or Import folder…) as one Folder card whose
  files loop through downstream nodes like a batch or go out as lists, after name filters, an order
  and a range, split-grid image previews (#337), per-node generation
  history strip, local pinned
  discussions with Note nodes (#68), grid/dependency formatting of selected nodes (#70), named
  colour tags with a bounded highlight filter and tag navigation (#67), router nodes that carry
  one modality as a cable tie (#72), fields: named, coloured zones that move with their members
  and collapse to their header (#66), which Delete removes (#708), the minimap draws as a header
  (#707), Fit all frames by that header (#739), and search and tag navigation open to reveal a
  hidden card (#706); shared parameter editing with explicit Mixed
  values and atomic undo across compatible selected nodes (#71), and an API keys panel backed by
  the OS keychain, whose late keychain answers still update the remembered accounts and which
  refuses a new account before writing when the list is full (#240). Shared model changes show
  per-target migration results before applying (#157).
  A mask editor paints brush masks over an image, expands its canvas and wires an inpaint or
  outpaint generation card from a prompt (#102).
  A Layer Editor card composites its wired images with text and shape layers into one frame;
  its full-screen editor moves, resizes, rotates, snaps and aligns layers (#101). Elements turn
  selected images into a named, reusable subject that prompts mention as `#name`, with a
  generated reference sheet (#104), and are saved to an Elements library every project shares,
  placed from it and updated from it (#552), even when placed under another name. A library
  Element can be renamed, shared, or exported as a bundle file another person imports (#765).
  The Techniques panel's Assets, History and Styles tabs browse the open project a page at a
  time: every image, video, sound, 3D model and PDF with a thumbnail, filtered by kind and
  inserted as a card; the open canvas's past results, restored or placed as a source card; and
  every trained style and Soul ID, marked with the provider that keeps it and wired into the
  selected card (`library.assets`, `library.history`, `library.styles`, part of #96). Ticked
  assets and results go on the canvas together in one undo step (`library.attach`) or are saved
  to a chosen folder, and Import files… brings picked files into the library as source cards,
  deduplicated by content, with a line per file and Retry failed (part of #96). Import from
  URL… fetches one media file from a public https address on the desktop, refusing local and
  private addresses on every redirect, capped at 16 MiB and checked by type and bytes, and keeps
  only its file name. Ticked files can be saved to My library, which every project shares
  (bounded at 4096 files and 1 GiB, repaired if its index is damaged), and placed from it on any
  canvas of any project (`library.saved`, `library.place_saved`; part of #96).
  Higgsfield Soul IDs are listed from the account, trained on a Train style card and kept as
  provider-bound handles: the Library and the Share live panel say they work only on the account
  that trained them, exports never write one out as a file, Use on selected card sets a SOUL 2
  card's Soul ID, and a project opened on another account keeps every setting and refuses the
  run before paying (#180, #680).
  An Export to folder card writes the results wired into it into a folder a person chose on the
  card, with a file-name pattern and a keep/number/replace policy, on the background export worker
  with Cancel; the card never runs with the canvas (#746, completing #98's batch export).
  An image card's Image tools menu lists inpaint, expand, remove background, upscale, split into grid or layers, color grade, Make Element and Open in
  3D, greyed with the reason where a tool can't be used, and adds the tool's card wired to the
  image as one undo step (#103). 3D models (glTF, GLB, OBJ, binary STL) become 3D nodes whose cards show
  the model in an orbit viewer drawn with wgpu, shaded, unlit or as a wireframe; Convert 3D
  writes a GLB or OBJ copy (#111), or geometry-only binary STL with bounded read/reopen support
  (part of #569; see `docs/3d-formats.md`). Built-in 3D cards save their viewer angle, elevation, zoom and
  shading in the project; a viewer gesture is one undo step and never changes generation inputs
  or existing captures (`mesh.view_set`, #928, part of #569). A custom shader card runs WGSL with declared sliders; its editor
  checks the code as it's typed, has a text model write or fix it through an ordinary, priced
  request card, and saves it to a versioned shader library every project shares, which places it
  on other canvases and moves cards to newer versions (#90). Typing `@` in a generation prompt mentions another card,
  whose text goes into the prompt where it is mentioned and which runs first, like a wire (#505).
  Present selection makes a Deck card with a slide per card and presents it full-window; slides
  are layer stacks whose source layers show what their card shows now, and `deck.*` commands
  list and edit them with revision checks (#122). Edit slides opens a slide sorter beside the
  Layer Editor's stage, to arrange each slide's text, shapes and sources (#694); its Export menu
  saves the slides as one PDF or as PNGs, drawn at the deck's size (`deck.export`, #692).
  Each deck holds imported fonts shared by its editor, thumbnails, presenter and exports; unknown or
  unreadable fonts visibly fall back to Lato, and `deck.list` resolves their references (#693).
  Deck cards show their first three slides as cached live thumbnails. Copying a deck together
  with its source cards makes those slides follow the copied sources (#695).
  A generation card's `− N× +` counter makes up to 20 variations a run, each its own request
  (quoted as such) and its own result in the node's history (#81).
  The viewer's Photo and Views buttons add a 3D photo or a
  4/8/16-view sheet card that draws the model from the current view as an image (#112). Each capture
  owns its device, and closing cancels and drains captures from live recompute and explicit runs
  within a documented 45-second capture budget (#603). Turntable adds a saved full-turn or back-and-forth video orbit, rendered one frame at a time with bounded pixel work (#575). PDFs become PDF cards showing the
  first page and page count; PDF to images draws a page range as images and PDF to text reads
  the text the pages draw (#113). A PDF reader pages through a PDF, sets a node's page range, and extracts a page or
  its text into new nodes (#590). The same bounded, undoable extractions are engine commands
  available to MCP and the control channel (#606). Scanned pages are recognised by a vision-capable text model: PDF to
  text headings mark pages that draw no text, PDF to images can draw only those, and the built-in
  chat models take up to 16 images on a text card (#592). They also read up to 5 wired PDFs
  directly, up to 100 pages a request, refused before sending when longer and with the pages'
  input tokens in the quote; a model that can't read PDFs points to PDF to text (#591).
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
  Luma Ray 3.2 text/frame video and Photon text-to-image reuse fal's queue and key (#26);
  Ray is quoted by duration/resolution and Photon keeps an unknown upfront megapixel charge. Run quotes also
  surface aggregate admission feedback before confirmation (#307). Imported video and audio
  sources probe duration (#608); ComfyUI accepts video and audio workflows (#110). Plugin cards
  keep their package identity, migrate with their package, and survive uninstall (#173). Plugin
  authors pack reproducible packages and lint them with the install verifier
  (`nodecraft-cli plugin pack|inventory|lint`, [`docs/plugin-authoring.md`](docs/plugin-authoring.md), #175),
  sign them with their own key (`plugin key new|trust`, `pack --sign`), and check them against a
  corpus of refused packages; a package may depend only on data packages (#866).
- **Batch Generate sheets** (M5, #163): the Sheets panel makes, names and reopens sheets, tables
  of Generate rows with their own prompt, model, aspect, settings and references, run together
  under one quote within budgets. Model and settings apply to the selected rows (Mixed values
  shown); "Retry failed" reruns only the failed rows, and running rows can be cancelled. A started
  run is a snapshot: later edits leave its results out of date. Drafts edit and save offline.
  "From template" expands a `{{variable}}` prompt (text values, an empty value, or image
  sources) into one row per combination; prompt and status filters narrow the rows. A Grid mode
  reviews the rows' results as badged thumbnails and steps through a re-run row's older results;
  the chosen results go to the canvas as source cards that record their row and run
  (`sheet.place`), or into a folder through the shared result save (macOS and Linux).
- **Actions** (M4): image action cards (blur, grade, filter, tint, flip, rotate, crop, key,
  replace colour, aspect, overlay, duplicate, resize, expand) and text action cards (split,
  replace, template, concat) come from quick-add and the palette (#85, #87, #410). After an edit
  they recompute on a worker, and while a slider drags they preview on smaller images before the
  full-size render on release (#88); video actions and the Timeline run only when asked. Split
  into grid cuts an image into tiles as new Image cards (#86). A card that would compute a result
  the project already holds for the same kind, settings and inputs reuses it: on by default for
  cards that don't call a model, opt-in per card with `node.cache_set` for those that do, and a
  run neither quotes nor pays for what it reuses (#91); the app has no button for that switch
  yet. Custom shader cards run their WGSL on the GPU (#90). The Plugins panel checks a
  `.ncraft-plugin` package, installs it disabled and, once granted and enabled, a text-action
  package's actions become cards that stop running when the grant is revoked (#172, #173); in
  the browser, packages last only for the session and never run (#823). Built-in image actions
  run on the CPU: the wgpu path for them (#89) is a tested library the app does not use yet.
- **Black Forest Labs**: desktop Generate image and the CLI can use the direct FLUX.2 klein 4B
  text-to-image adapter (#33), with a keychain API key, resumable polling and bounded PNG downloads.
  Pixel-based credits remain an unknown monetary price and require consent. Image editing and
  other direct BFL models are not yet included; see [`docs/providers.md`](docs/providers.md).
- **MiniMax**: H3 text/first-frame video, Speech 2.8 HD and Music 3 share Fal's key and queue
  (#27), with explicit priced settings and native streamed storage. See [provider limits](docs/providers.md#minimax-video-speech-and-music-through-fal-27).
- **Pika**: desktop projects and the native CLI animate one first frame with a prompt using
  current Pika 2.5 through fal (#29), at 720p for 5 or 10 seconds with explicit priced consent.
  Browser sessions refuse before payment. Text-to-video and keyframes are not included; see
  [`docs/providers.md`](docs/providers.md#pika-25-through-fal-29).
- **HeyGen**: desktop projects and the native CLI animate a photo or lip-sync an MP4 with
  supplied speech through fal (#43). Monetary pricing remains unknown and requires consent;
  see [`docs/providers.md`](docs/providers.md#heygen-through-fal-43) for client limits.
- **Magnific**: desktop projects and the native CLI can enhance one image with Precision V2
  photo processing, 2×/4× scaling and three enhancement controls (#38). Organizational credit
  spending remains an unknown monetary quote and needs explicit consent. Browser and Classic
  in-memory storage refuse before payment; see [`docs/providers.md`](docs/providers.md).
- **Topaz**: desktop projects and the native CLI can enhance or upscale one image with
  Precision Standard V2 through fal (#37). Integer 1×–4× scaling stays within one 0.08 USD
  output block. Browser sessions refuse before payment; see [`docs/providers.md`](docs/providers.md).
- **Video and audio on desktop**: installed FFmpeg and ffprobe power video card playback and
  scrubbing (#105), waveforms and sound playback (#106, #666), video actions (#108) and Timeline
  renders (#109). The Timeline joins clips with fades and crossfades, mixes audio tracks, overlays
  picture tracks and captions, and gives each picture clip placement, crop, opacity and speed
  controls (#574, #576, #577). It warns about trims that no longer fit regenerated sources and
  records the source versions used in a render (#578). It exports FCPXML with source media and a
  SubRip companion when captions show (#166, #649). See [setup](docs/install.md#optional-ffmpeg-tools)
  and [rendering and playback](docs/media-video.md).
- **Video and audio generation**: Generate video supports model-specific text, frame, reference
  and speech inputs (#559, #607). Generate audio runs text-to-audio and audio-to-audio models (#672)
  and plays the result. ComfyUI workflows can return video and audio (#110). Model support and
  credentials vary by provider and host. See [provider capabilities](docs/providers.md).
- **Document integrity**: every edit is an invertible changeset; undo/redo with a bounded stack;
  command dispatch under a panic guard; a shared registry of stable dotted commands with typed JSON params.
- **Automation**: `nodecraft-cli mcp` (MCP over stdio or, with `--listen`, Streamable HTTP on
  loopback with a bearer token; 4 generic tools), `mcp --bridge` to drive a running app, the
  desktop app's own MCP server (Automation › Enable MCP server, #116), `nodecraft-cli control`, and `nodecraft --control` (local JSON control channel with
  screenshots), plus `nodecraft-cli serve` for a project's published Techniques over a local
  authenticated HTTP API with optional signed webhooks. Agents open and save projects only inside
  the folders the person allows with `--root`/`--read-root` (#423). See [`docs/mcp.md`](docs/mcp.md),
  [`docs/control.md`](docs/control.md) and [`docs/serve.md`](docs/serve.md).
- **Headless CLI**: named project/Technique inputs, isolated CSV rows with aggregate quotes and
  spending limits, provider credential checks and ComfyUI workflow import (#97); see
  [`docs/cli.md`](docs/cli.md). The planner, scheduler, driver, run executor, cost quotes, budgets, usage ledger,
  staleness hashing and router resolution exist and are tested end to end against the mock
  provider.

- **Live sharing** lets people edit the same canvas through `nodecraft-cli relay` (#119).
  Open **Share** on desktop or in the browser to start a room or join with a link. Desktop Join
  opens a window, while browser Join uses the current tab and asks you to resolve unsaved work
  first (#698, #699). Links grant view, comment or edit access with expiry and revocation (#121).
  Private canvases stay out of the room. Named cursors, selection outlines and initials show
  who is working where (#120). Desktop peers can transfer large assets in parts (#700).
  Browser peers keep assets within the tab's memory limits.

## What does not work yet

The honest list, because "it's in the repo" and "you can use it" are different claims:

- **Generation is not finished across every model or surface.** The desktop run host now constructs
  supported catalog adapters and runs authorized generation cards (#409, #362, #83); a listed model
  is not proof that every provider, modality or input mode runs in every host. The text card's
  turn UI, mentions, streaming, connected-media thumbnails, video to text (Gemini 3.8 Flash,
  inline videos under 20 MB) and built-in models' declared context windows have landed (#84).
  The built-in windows are far larger than a card's prompt can be, so an over-long request to
  them is refused by the provider, not by planning; browser provider settings/option lists differ from
  desktop (#721). Check [`docs/providers.md`](docs/providers.md) for adapter-specific limits.
- **Local actions work in the app, on the CPU.** Image/text action node kinds (#410) recompute on
  a worker after edits (#88); slider drags preview with smaller images before full-size rendering.
  The browser has no equivalent worker threads and runs rounds in the frame. Built-in image
  actions don't use the GPU path (#89) yet, and custom shaders need a GPU. Result caching for
  model cards is a per-card command (`node.cache_set`) with no button yet (#91). Module
  (`rust_wasm`) plugin packages are refused at install; provider-template plugins are M5 work
  whose tracker (#174) is still open. This is not evidence that all paid provider operations or
  video/audio actions work in every host.
- **The in-canvas agent works in the desktop app.** Cmd+/ opens the agent sidebar (#114): ask in
  words, with the selection and @-mentioned nodes as context. `crates/agent` runs the model's tool
  calls over the MCP tools, judging each one first (`Session::impact`): assist mode asks before any
  change or run, auto mode runs edits and runs within the `[agent]` budget. The endpoint (Anthropic,
  OpenAI, OpenRouter, Ollama, LM Studio) and model are set in `[agent]` in the settings file; there
  is no settings screen for them yet, and the browser build has no agent.
- **The browser build is a preview.** `apps/nodecraft-web` runs the canvas in a browser and
  releases ship it as `nodecraft-web-<v>.zip` (#99). Provider keys are kept in the browser's
  storage for the site, and projects open and save through the browser's file pickers or as
  downloads (#118). Cards run in the browser too, calling the cloud providers that accept
  requests from web pages directly with those keys (#118). **Providers** saves ComfyUI and
  LiteLLM addresses per site. Browser option lists fetch in the background when the server allows
  the app's origin (#721). Supported catalog models can also use configured custom endpoints
  (#960). Video playback and rendering use desktop FFmpeg. The browser offers recovery from
  site storage (#717) and asks before leaving with unsaved work. Browser files are read
  asynchronously and added as nodes (#319).
  The browser can paste PNG images (#633).

- **Desktop media limits.** Card video playback is silent. Sound cards play separately through the
  default output device. Playback and FFmpeg-backed renders run on desktop. Timeline
  interchange is available on desktop, with Final Cut Pro and DaVinci Resolve import checks and
  background export improvements tracked separately (#643).

- **Relay transport and storage differ by client.** The relay serves plain `ws://`.
  Desktop clients use that transport. A browser served over HTTPS needs a TLS proxy for a
  remote relay. The [installation guide](docs/install.md#live-sharing-in-the-browser) covers
  local development and remote access. The relay holds rooms in memory.
- **Remaining gaps are specific, not whole missing milestones.** Batch, Techniques, app mode,
  the Layer Editor, Elements and PDF cards have shipped in some form. The first signed release
  (#100) and typed nested descriptor records/lists (#528) remain open. HTTP serve
  (`nodecraft-cli serve`) runs Techniques with signed completion webhooks, idempotent retries
  and budget caps (#117). See [the serve guide](docs/serve.md).
  Provider availability and modes vary by adapter and host: Google Gemini Omni Flash video
  replaces the shut-down Veo API (#17), and Veo 3.1, Fast and Lite run on Google Cloud with a
  service account (#742), as do Google TTS's classic and Gemini voices (#21) and the Gemini
  image models (#47); Seedance 2.5 and
  2.0 make video on BytePlus ModelArk (#18); Kling's own task API makes video and images with a
  Kling key (#19); most `future` adapters remain tracked separately. ElevenLabs speech (v3,
  Multilingual v2, Flash v2.5), effects, music and voice changer models (#20) have a Generate
  audio card (#672), account voice choices (#670), and streaming speech playback on desktop
  (#671, #875). The CLI and browser do not request streaming previews. 3D has Tripo
  text/image/multiview and fal Trellis (#22), plus Meshy
  text/image/multi-image/retexture/remesh (#23). Consult [`docs/providers.md`](docs/providers.md)
  for model and host capabilities. Published releases track their tagged source.

## Next up

The near-term order follows the remaining open issues, not an already-finished wiring checklist:

1. **Finish M3 generation**: ComfyUI workflow field mappings (#948) and the M3 close-out of the
   README, ROADMAP and docs (#374); do not mistake a tested adapter for a working mode in every host.
2. **Finish reusable delivery**: complete the remaining M5 work and first signed release (#100),
   including provider plugins (#174).
   M4's open follow-ups sit outside a milestone: the Generate confirmation should say which nodes
   reuse a cached result (#811), and a superseded live action should be cancelled and its work
   bounded (#596). Using the wgpu path (#89) for built-in image actions, and a button for result
   caching on model cards, have no issue yet.
   For the changing priority order, use the [open milestone issues](https://github.com/CypherNaught-0x/nodecraft/milestones).

## Milestone log

- **2026-11-03** — M10 closed with #381. Desktop and browser Share and Join, role-scoped links,
  presence, bounded asset transfer and deck editing/export are in the app. The real relay
  integration test covers two peers editing one canvas and a late joiner.

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
  GitHub still had #372 open until its documentation close-out landed.
- **2026-10-23** — M1 closed with its documentation close-out (#372). M2 (Organise) feature work
  finished: fields gained Delete (#708), stored-geometry membership and shift-drag box select
  (#709), reveal-on-search and tag navigation (#706), minimap headers (#707) and header-only Fit
  all (#739), alongside the earlier tags, comments, search, auto-format, bulk panel and router
  work. The M2 close-out (#373) refreshed README, ROADMAP and `docs/plan.md`. M6 gained the
  Elements library (#552) and the Image tools menu (#103); providers gained Veo 3.1 on Google
  Cloud (#742) and Google TTS (#21).
- **2026-10-24** — counts refreshed from GitHub's completed-only closures: M5 18/30, M6 10/13,
  M7 28/31, M8 9/13, M10 14/17 and Future providers 3/29. The per-milestone close-outs (#374,
  #376–#382) remain open: each stays open until its milestone's feature issues close.
- **2026-10-23** — spending caps per call, session and project are settings the Budgets panel
  edits (#201); a run a cap blocks names it and its confirmation opens the panel. No command can
  raise a cap.
- **2026-10-23** — the spending log records each charge's project, so the open project has its
  own cap beside the total (#847); the CSV export gains a `project` column.
- **2026-10-24** — opening or recovering a located project with the old starter identity assigns
  a stable identity in one undoable change (#879), separating future project spending. Save As
  preserves identity; saved jobs under the ambiguous legacy identity remain parked with an
  explicit explanation, and their charges stay attributed to that legacy identity.
- **2026-10-24** — Replicate creation propagates a bounded server lifetime and successful results
  retain reported execution milliseconds through saving, held results and restart recovery
  (#339). Duration remains separate from money; restored handles do not reset the server limit.
- **2026-10-23** — rendered videos and extracted sound share a per-run staging budget (#584):
  1 GiB and 4,096 results, including read-backs in flight. Failure and cancellation return their
  reservations; chained actions retain access to earlier completed outputs.
- **2026-10-23** — M4 (Actions) feature work finished. Fourteen image and four text action kinds
  run as cards (#410) and recompute live on a worker, previewing on smaller images while a slider
  drags (#88); results are reused by input hash, on by default for local cards and opt-in for
  model cards, and runs neither quote nor pay for reused nodes (#91); custom WGSL shader cards
  can be written by a text model through an ordinary priced card and kept in a versioned shader
  library (#90). The plugin store verifies, installs, grants and revokes packages, and granted
  text-action packages run as cards (#172, #173), with session-only packages in the browser
  (#823) and structured property tests over hostile archives and failing operations (#824). The
  wgpu image path (#89) remains a tested library the app doesn't use for built-in actions. The M4
  close-out (#375) refreshed README, ROADMAP and docs. Elsewhere, Kling's own task API (#19),
  browser live sharing (#699) and several ComfyUI servers per install (#864) landed.
- **2026-10-23** — card option pickers filter labels and values, open at the current choice,
  keep a stable width and clear stale overrides with Use default (#678). Indexed selected labels,
  virtual rows and bounded filter work support fetched lists of up to 16,384 choices.
- **2026-10-28** — the Batch node is complete (#92): a batch collecting another batch flattens
  its items into its own (bounded at 1,000 items and 16 batches per chain, every item quoted
  before the run), and "Review items…" on a fanned card steps through each item's result, with a
  rows × columns grid for a Cross of two batches that marks failed and cancelled cells. Batches
  inside a technique's own graph are not yet supported (#93). M5 25/33.

- **2026-11-03** — M7 close-out (#378). The Timeline run host rendered four one-second picture
  clips with audio, clip fades and three crossfades into one WebM. Desktop playback, rendering,
  generation and interchange coverage are recorded in [the M7 checklist](docs/plan.md#7-roadmap).
