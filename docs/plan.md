# NodeCraft plan

> **Status:** draft 2 (owner decisions folded in) · **Date:** 2026-10-16 · **Scope:** product feature set, architecture,
> repository setup and roadmap for NodeCraft, a native node-based canvas for generative media.

NodeCraft is a node-based infinite canvas where text, image, video, audio, 3D and document nodes
connect to each other and to any AI model, alongside deterministic editing tools, so that a
creative process can be seen, branched, repeated and packaged as a reusable tool. It is written in
Rust only, renders with egui/eframe on wgpu, runs on macOS, Windows and Linux (with a browser
build of the same code later), works offline-first with local project files, and every action is
reachable headless from the CLI, a control channel, MCP and an in-canvas agent.

---

## 1. What we learned from the research

### 1.1 Sources studied

| Source | What it contributes |
|---|---|
| Flora canvas (product pages, docs, "Intro to the Canvas" workshop, product screenshots) | The most complete creative node canvas: node types, run semantics, generation history, batch and router nodes, Techniques, Elements, Layer Editor, Timeline Editor, 3D node, actions and custom actions, in-canvas agent, fields, tags, comments, multiplayer |
| Wireflow (feature index) | Per-node cost shown before running, spend caps, model swap without rewiring, batch from CSV or prompt lists, every published workflow becomes a REST endpoint and MCP tool, templates |
| PhotoCraft (`storytold/photocraft`) | How to set up the repository: layered Cargo workspace enforced by `xtask`, engine-first and headless-first, "everything is a command", thin data-driven egui shell, no-panic and no-unsafe lints, wasm-safe core crates, `AGENTS.md` + `docs/` conventions |

We study these products for **behaviour and UX only** (clean-room). No code, assets, icons or
branding are copied.

### 1.2 What the screenshots show (UI details worth keeping)

- **Dark, dot-grid infinite canvas.** Cards with large radii. A small uppercase **label above
  each node** (`GLASS FLOWER`, `COLOR PALETTE`). Smooth bezier "noodles". One **`+` output
  handle** on the node edge that both drags new connections and fans out to many targets.
- **Hover toolbar above the selected node:** expand/fullscreen, bookmark, download, presets,
  aspect ratio (`9:16`), style/strength, duplicate, model picker (`Flux Dev ▾`).
- **Prompt overlay inside media nodes:** the prompt sits on the image with a submit arrow; a
  vertical **variation counter** (`+ 4 −`) sits next to Generate.
- **"Turn into" menu** appears when a noodle is dropped on empty canvas: Text / Image / Video …
  with single-key shortcuts and ↑↓/↵ keyboard navigation.
- **Text nodes behave like a chat turn:** the instruction sits at the bottom ("Can you pull some
  hex colors from this image?"), the LLM answer above, with the connected image as a thumbnail.
- **Model picker:** "Search compatible models" (filtered by what is wired in), toggles for
  **Auto select model** and **Use multiple models**, models grouped by provider; each model row
  shows a quality rating (dots), typical latency (`15s`, `1m`), accepted input icons and a
  one-line description.
- **Left rail:** add node, elements/shapes, history, techniques, comments, help, account.
- **Top right:** collaborator avatars, **Play** (app mode) and **Share**. Collaborators' named,
  coloured cursors move live on the canvas.

### 1.3 Workflow patterns the workshop teaches (the product must make these effortless)

1. **Combine:** two sources → one generation node ("blue square + green circle = ?").
2. **Branch:** drag from one result into three new nodes with different edits; nothing waits —
   other branches can be started while one generates.
3. **Prompt helper:** image → text node (LLM writes the prompt) → image node, with the
   original image *also* wired in. A connected text node replaces the prompt and locks it.
4. **Variations:** the same node run N times; results go to node history or out onto the canvas
   (user preference).
5. **Prompt fan-out:** N text-node variations with different instructions, each feeding an image
   node; select all → Enter runs them in dependency order.
6. **Composite then re-render:** remove background → Layer Editor (place, scale, add type,
   shapes) → image node "make it seamless", or "mock this up on a brochure".
7. **3D for camera control:** image → 3D model → orbit → capture photo / turntable video /
   4–16-view sheet → re-render in the original style, or turn the views into an Element.
8. **Video:** still image → text node writes motion prompt → video node; several short clips →
   Timeline node (order, trim, fades, audio) → render.
9. **Package:** a workflow with clear inputs and outputs → **Technique** (one node, or an app
   form) → share; run from canvas, app mode, CLI, API or agent.
10. **Organise:** fields (groups) with colours, colour tags with a "jump to tagged" filter, router
    nodes as cable ties, auto-format of a selection (not of a whole workflow), canvas search.

---

## 2. Feature set: the best of each, plus what they lack

Legend: **M#** = roadmap milestone where it lands (§7).

### 2.1 Canvas and editing

| Feature | Notes | M |
|---|---|---|
| Infinite canvas, pan/zoom, dot grid, minimap | Middle/right-drag or scroll to pan, Ctrl/Cmd+scroll or pinch to zoom, zoom-to-fit, zoom to selection | 1 |
| Selection | Click, Shift-click, box select, select upstream/downstream chain, select all of a type | 1 |
| Noodles | Bezier, typed colours per modality, animated while the upstream job runs, click/box-select and Delete to remove, reroute by dragging an end | 1 |
| Drop-on-empty "Turn into" picker | Only offers node kinds that accept the dragged modality; keyboard driven | 1 |
| Quick-add shortcuts | `T` text, `I` image, `V` video, `A` audio, `3` 3D, `R` router, `F` field, `B` batch, `L` layer editor, `C` comment, `Shift+T` technique picker, double-click canvas for a searchable palette | 1 |
| Copy / paste / duplicate | Nodes, fields and edges; paste images, video, audio, PDF and text from the OS clipboard as source nodes; drag-and-drop files from the OS | 1 |
| Undo / redo | Every edit is a reversible changeset (§4.3) | 1 |
| Fields (groups) | Named, coloured containers; moving a field moves its members; empty fields as layout zones; collapse | 2 |
| Colour tags | Per node, multi-select tagging, and a tag filter that highlights matches | 2 |
| Comments | Pinned comment bubbles and threads on canvas or node | 2 (local), 9 (shared) |
| Canvas search | `Ctrl/Cmd+F` by title, prompt text, model or tag | 2 |
| Auto-format | Tidy a selection into a grid or a left-to-right layered layout | 2 |
| Bulk parameter panel | Multi-select nodes of one kind: shared parameters only, "Mixed" indicators, change model for all | 2 |
| Input reordering | Drag connected inputs to set order ("first image", "second image") — order is shown as numbered badges on the noodle ends | 1 |
| Prompt overlay toggle | `H` hides prompt overlays on media nodes for a clean board | 1 |
| Node titles | Editable label above the card; default from the prompt | 1 |
| Presentation / Decks | Present selected nodes or fields as slides | 10 |

### 2.2 Node catalogue

**Generative nodes** (one per modality; the model is a parameter, never a different node kind):

| Node | Modes (by inputs wired) | Highlights |
|---|---|---|
| **Text** | text→text, image→text, video→text, audio→text, PDF→text | Chat-style turn UI; output is text that can drive downstream prompts; `@node` and `#element` mentions |
| **Image** | text→image, image(s)→image (edit, combine, restyle) | Aspect ratio, resolution, effort/quality, seed, N variations; tools menu (below) |
| **Video** | text→video, image(s)→video (first/last/reference frames), audio→video | Duration, resolution, audio on/off; playback with scrubber |
| **Audio** | text→speech, sound effects, music; voice options | Waveform preview, playback |
| **3D** | text→3D, image→3D, 3D→3D (remesh, retexture, segment) | Orbit viewer; **capture photo**, **record turntable/orbit video**, **view sheet** (4/8/16 views); export GLB/OBJ |
| **ComfyUI Workflow** | Whatever the imported workflow does | Ports and parameters generated from the workflow's mapped inputs/outputs; runs on any configured ComfyUI server (§2.5.1) |

**Source and structure nodes:**

| Node | Purpose |
|---|---|
| **Source** (image/video/audio/3D/PDF/text) | Imported or pasted media; the same card UI as generated media, just without a model |
| **Document** | PDF → pages as images, or extracted text |
| **Router** | Transparent pass-through hub: one set of sources, many consumers; resolved away at plan time (adds no cost or step); single modality, renamable |
| **Batch** | Collects N items (from upstream nodes, uploads, CSV first column, or text split by line/custom separator) and fans each through downstream nodes; one modality per batch; two batches into one node run **Cross** (N×M) or **Zip** (min(N,M)); outputs tagged with their source item |
| **Element** | A reusable subject reference (product, character, material, setting, style, moodboard…): up to 8 images + category + guidelines → a curated reference sheet; usable as an input node or as a `#mention` in prompts; falls back to raw images if no sheet exists |
| **Technique** | A published subgraph used as one node, with named, typed inputs and outputs (§2.4) |
| **Export** | Writes results to a folder or a naming pattern (local first; cloud drives later) |
| **Comment / Note** | Free text on canvas |

**Editor nodes** (they open a full-screen editor in place; their output is a normal medium):

| Node | Editor |
|---|---|
| **Layer Editor** | Compositing: each connected visual becomes a layer; layers panel (visibility, lock, rename, reorder), move/scale/rotate/flip, align, snap, opacity, text layers (font, size, leading, tracking, custom font upload), shapes (rect/ellipse/triangle, fill/stroke, hex input), frame size 100–4096 px; output = composited image |
| **Timeline** | Video sequencing: order clips, trim, fades in/out, cross transitions, per-clip volume and fades, captions; output = rendered video |
| **Mask / Inpaint** | Brush mask over an image (size, hardness), prompt → new node with the edit (inpaint / outpaint) |

**Action nodes** (deterministic, local, free; they run as soon as their inputs change):

- **Image:** colour grade (warmth, contrast, saturation, brightness), colour filter presets
  (grayscale, sepia, duotone, noir), tint with blend modes, key/replace a colour, change aspect
  ratio (crop / pad with colour or blur), rotate, flip, blur family (gaussian, box, motion,
  radial, bilateral, bokeh, tilt-shift), crop, resize, **split into grid** (pick N×M visually →
  N×M image nodes), overlay/watermark, duplicate N.
- **Video:** grade, filter, effects (vignette, grain, pixelate, shake, chromatic aberration,
  VHS), stitch, split (equal parts, fixed duration, scene detection), extract frames (single,
  even, interval, scene change), Ken Burns from a still, frame grid / contact sheet, long
  exposure, boomerang, reverse, speed up / slow down, green-screen key, extract/replace audio.
- **Text:** split (separator, regex, paragraph, line count, chars), find/replace (regex, case,
  whole word), concat (separator, prefix, suffix, trim, skip empty), template fill.
- **Custom actions:** describe a filter in words → the agent writes a **WGSL shader with declared
  parameters** (image/video) or a small sandboxed **WebAssembly/expression** program (text) → a
  new action node with generated sliders. Shaders are sandboxed by construction and run on the
  GPU; custom actions can be saved to the library (Flora cannot yet save them).

**Image node tools menu** (shortcuts that create the right node, already wired): inpaint,
outpaint/expand, remove background, upscale, split into grid, split into layers (AI, 2–5 layers →
Layer Editor), colour grade, all image actions, create Element from selection, open in 3D.

### 2.3 Running nodes

The semantics take Flora's explicit, cost-aware model for paid generations and add what it
lacks: staleness tracking, optional caching, and live deterministic nodes.

- **Explicit runs for generative nodes.** Generate arrow, Enter in the prompt (Shift+Enter =
  newline), or select nodes + Enter. Multi-select runs in **dependency order**; the tooltip shows
  node count and **total estimated cost** before you confirm.
- **Run modes:** *Run selected* (upstream must already have results; otherwise "can't run yet"
  with the missing input highlighted), *Run with upstream* (also runs stale or empty
  ancestors), *Run field*, *Run all stale*.
- **Staleness instead of silent drift.** Each generation records the content hash of the
  inputs it consumed. When an upstream result or parameter changes, downstream nodes are not
  re-run (it costs money) but get a **stale badge** and can be refreshed in one click.
- **Live action nodes.** Deterministic local actions recompute on input/parameter change, with
  cheap previews while dragging a slider and a full-resolution render on release.
- **Caching (opt-in per node or project):** identical (model, params, inputs, seed) can reuse a
  previous result instead of paying again. Default off for generative nodes, so re-running gives
  a new take as users expect.
- **Variations:** `+/−` counter up to 20 per run; preference for where they go: node history
  (default) or new canvas nodes laid out beside the source. **Use multiple models** runs the same
  inputs on several models at once (default: canvas nodes, one per model).
- **Status:** queued → generating (progress and ETA where the provider reports it) → done /
  failed (error card with cause and a fix: blocked content, unsupported input, model busy,
  timeout, budget, network). Cancel a node or all in a selection.
- **Generation history per node:** thumbnail strip, ←/→ to browse, click to make **active** (the
  active one is what downstream uses), copy, export, delete, extract to canvas, "clear node"
  with undo. A global **History** library across projects.
- **Queue panel:** every job in the session with status, provider, cost, retry and cancel.
- **Budgets:** per-project and per-session spend caps, hard per-call caps, cost shown on the
  Generate button and in run confirmations; usage log exportable as CSV.

### 2.4 Reuse: Techniques, Elements, templates, library

- **Technique Builder** (Intro → Inputs → Outputs → Publish): pick input nodes (no incoming
  edges, has content) and output nodes (no outgoing edges); everything in between is included by
  tracing back from the outputs. Each exposed port gets a name, short description and the
  current content as preset/example. Optional **user-editable parameters** on outputs (override
  model/settings). Validation: connected, acyclic, compatible types, configured, **no explosive
  batch fan-out** (bounded N×M×… estimate). Publish metadata: name, description, category, tags,
  thumbnail, estimated cost, version.
- **Technique node:** collapsed green node on canvas; "view workflow" expands it read-only;
  "edit technique" expands it for editing and republishes a new version in place.
- **App mode** (the ▶ button): a generated form with the technique's inputs, cost, Run, and the
  outputs gallery — usable by people who never see the graph. In the browser build this is a
  shareable page.
- **Everything a technique is, is also an API:** `nodecraft run <technique> --input name=path`,
  an MCP tool per technique, and an HTTP endpoint in `nodecraft serve` (Wireflow's best idea).
- **Elements library** (private / shared), reference history, regenerate, default reference.
- **Templates / flows:** starter graphs (combine, branch, prompt helper, storyboard, layout
  resizer, character turnaround, colour transfer, product shot) shipped as techniques you can
  open and edit.
- **Assets & History browsers:** everything imported or generated, filter by type, drag back
  onto any canvas, across projects.

### 2.5 Models and providers

- **Model catalog as data** (TOML/JSON shipped and updatable): id, provider, modality,
  accepted inputs (counts and roles, e.g. first/last frame), parameters schema, resolutions,
  aspect ratios, durations, pricing formula, typical latency, quality tier, description.
  Adding a model is a data change, not code.
- **Compatible-model search:** the picker filters by what is wired into the node; incompatible
  models are disabled with the reason.
- **Model swap without rewiring;** parameters are migrated by name, unknown ones dropped with a
  visible notice.
- **Auto select model:** a rule-based (and later agent-assisted) choice from the intent, inputs,
  quality/effort, and budget.
- **Bring your own keys,** stored in the OS keychain, never in project files.

**Provider set (decided 2026-10-16).** The milestone column says when each adapter ships. Every
provider not listed here is tracked as a GitHub issue labelled `area:providers` + `future`
(§2.5.2).

| Provider | Models / endpoints | Modes | Notes | M |
|---|---|---|---|---|
| **ComfyUI** (first-class, see §2.5.1) | Any local or remote ComfyUI server | Whatever the workflow does: image, video, audio, text, **3D** (e.g. Hunyuan3D, TRELLIS and other mesh workflows that save GLB/OBJ) | Free and offline; no cloud account needed | 3 (3D outputs: 8) |
| **Comfy Cloud** | Hosted ComfyUI | Same Workflow node and ComfyUI-backed catalog models, run without a local GPU | Separate backend profile (auth, upload, queue differences); the exact API surface is confirmed in its issue | 5 |
| **Aggregators: fal.ai, Replicate** | Hundreds of hosted image, video, audio, 3D and upscale models | Every mode; models added as catalog entries pointing at the aggregator's model id | Broadest coverage with one key each; queue/webhook-style async APIs | 3 |
| **OpenAI Images** | GPT Image family | text→image, image(s)→image edit, mask inpaint | Sizes, quality, background transparency, N | 3 |
| **Google** (Gemini API, Vertex AI later) | **Imagen** (generate, edit), **Gemini image** models, **Veo** (video) | text→image, image(s)→image; text→video, image→video (first/last frame), audio on/off | Long-running operations polled | 3 (Veo: 7) |
| **Google TTS** | Cloud Text-to-Speech (Neural2/Chirp voices) and Gemini TTS | text→speech; voice, language, speaking rate, pitch, SSML; multi-speaker on Gemini TTS | Same Google credentials as above | 7 |
| **ElevenLabs** | Text-to-speech, sound effects, music, voice changer | text→speech, text→SFX, text→music, audio→audio | Voice picker loaded from the account; streaming previews | 7 |
| **ByteDance** (BytePlus ModelArk) | **Seedream** (image), **Seedance** (video) | text→image, multi-reference image edit; text/image→video, reference mode | Async task API | 3 (Seedance: 7) |
| **Kling** | Kling video (and image) models | text→video, image→video (start/end frame), multi-image reference, lip sync later | JWT-signed requests; async task API | 7 |
| **Tripo AI** | Tripo 3D generation API | text→3D, image→3D, multiview→3D, texture, rig/animate later | Async tasks; GLB/FBX/OBJ outputs | 8 |
| **Meshy** | Meshy 3D API | text→3D, image→3D, retexture, remesh | Async tasks; GLB/OBJ/FBX/USDZ outputs | 8 |
| **LLM endpoints** | **OpenAI-compatible Chat Completions** (OpenAI, Ollama, LM Studio, vLLM, OpenRouter …), **Anthropic Messages**, and **LiteLLM** (proxy in front of 100+ LLM providers, plus its image and speech routes) | text→text, image→text (vision), streaming | Powers Text nodes, prompt helpers, auto-select and the agent. LiteLLM has its own profile (virtual keys, model list from `/models`, spend from its API) so a team can route everything through one gateway | 3 |

Each adapter lives in `crates/providers/src/<provider>/` behind its own Cargo feature. Its
models live in the catalog data, so new models from the same vendor need no code. The
aggregator adapters reuse the shared async-task machinery (submit → poll/webhook → download),
which the direct video and 3D vendors use too.

#### 2.5.1 ComfyUI as a first-class citizen

ComfyUI is the main local engine, not just one more HTTP endpoint:

- **Servers panel:** add local or remote servers (URL, optional auth header). Health check,
  queue length, GPU/VRAM from `/system_stats`. A server can be marked as the default local
  backend.
  **Comfy Cloud** appears in the same panel as a hosted backend, so a workflow node can switch
  between the local GPU and the cloud.
- **3D workflows:** mesh-producing workflows (Hunyuan3D, TRELLIS, …) map their GLB/OBJ outputs to
  `Mesh` ports and open in the 3D node's viewer, just like Tripo or Meshy results.
- **Workflow node:** import a ComfyUI workflow (API-format JSON, or the UI format converted
  through the server's `/object_info`). NodeCraft finds the exposable inputs (`LoadImage`,
  `LoadVideo`/`LoadAudio`, text encoders, seeds, steps, CFG, sampler, checkpoint/LoRA pickers)
  and outputs (`SaveImage`, `PreviewImage`, video/audio save nodes). It turns them into typed
  ports and a generated parameter schema, with enums filled from `/object_info`. You pick which
  widgets to expose, and their names and order, in a small mapping editor. The mapping is saved
  with the node.
- **Catalog entries backed by ComfyUI:** a catalog model can name a ComfyUI workflow template
  instead of a cloud API (e.g. "Flux Dev (local)"). It then shows up in the normal Image/Video
  node model picker, and Image nodes can switch between cloud and local without rewiring.
- **Execution:** upload inputs (`/upload/image`, and so on), `POST /prompt` with a client id,
  live progress and previews over the WebSocket (`progress`, `executing`, `executed`), fetch
  outputs via `/view` into the asset store, and cancel through `/interrupt` or a queue delete.
  The planner honours per-server concurrency, and batch items queue as separate prompts.
- **Model awareness:** list checkpoints, LoRAs, VAEs and ControlNets from the server for pickers.
  Show a clear error card (not a crash) when a workflow references missing custom nodes or
  models.
- **Round trip:** "Open in ComfyUI" exports the mapped workflow with current inputs baked in.
  Techniques can contain ComfyUI workflow nodes and run headless from the CLI against any
  server.
- **Later:** inline individual ComfyUI nodes as NodeCraft nodes (generated from `/object_info`)
  for people who want to wire Comfy graphs directly on the canvas.

#### 2.5.2 Future providers (tracked as issues)

These are worth adding once the adapter pattern has settled. Each one gets a GitHub issue
(`area:providers`, `future`) so contributors can `/claim` it:

- **Video:** Runway, Luma (Ray), MiniMax Hailuo, OpenAI Sora, Pika, Lightricks LTX, xAI Grok
  Imagine, Alibaba Wan (DashScope)
- **Image:** Black Forest Labs API (FLUX direct), Ideogram, Recraft, Stability AI, Luma Photon,
  Alibaba Qwen-Image
- **Upscale / enhance:** Topaz, Magnific
- **Audio:** OpenAI TTS and transcription, MiniMax speech and music, Stability audio, Deepgram
  (speech-to-text)
- **3D:** Rodin (Hyper3D), Tencent Hunyuan3D cloud, Stability 3D
- **Avatars and lip sync:** HeyGen
- **LLM and gateways:** native Gemini API (beyond OpenAI-compatible mode), Azure OpenAI, AWS
  Bedrock and Google Vertex AI (also reachable through LiteLLM), Hugging Face Inference
  Providers

### 2.6 In-canvas agent

- A sidebar chat (`Ctrl/Cmd+/`, docked or floating) that **acts on the canvas** through the same
  engine commands as the UI: build a workflow from a description, add nodes connected to the
  selection, change prompts/models/settings, rewire, group, lay out, write custom actions, and run
  nodes.
- Context: the current selection (bounded), `@node` mentions, attached images.
- **Assist mode** (default): shows the planned nodes, runs and cost and waits for approval.
  **Auto mode:** runs immediately within the budget cap.
- Steps shown as a timeline; Esc cancels; per-project session history.
- Provider-agnostic LLM tool loop; Anthropic Claude as the default backend, local models
  supported.

### 2.7 Collaboration (later, designed for from day one)

- Every mutation is already a serialisable changeset (§4.3), so live sync is "ship changesets",
  not a rewrite. Plan: CRDT document (`yrs`/Yjs-compatible or `automerge`) behind the
  `graph` changeset API, a small relay server, presence (named cursors, avatars, selections),
  shared comments, share links with roles (view / comment / edit), private canvases.

---

## 3. Engineering principles (adopted from PhotoCraft)

1. **Engine-first, headless-first.** The GUI is one client of the engine. Every feature works
   from tests, the CLI, the control channel and MCP.
2. **Everything is a command.** Each user-visible action has a stable id
   (`node.add`, `edge.connect`, `run.selected`, `technique.publish`, …) with typed, serialisable
   params, `enabled` and `run`. Menus, shortcuts, the palette, CLI, MCP and the agent dispatch
   commands by id.
3. **One mutation path.** Commands produce **changesets** applied to the document; changesets
   are invertible (undo), serialisable (autosave journal, collaboration, agent audit trail).
4. **Data describes the UI.** Node kinds declare ports and a **parameter schema**; the inspector,
   node body controls, bulk panel, app-mode form and MCP tool schema are all generated from it.
   A new node kind writes no bespoke UI unless it wants a custom body.
5. **No modality assumptions.** Port types and values are runtime data; nothing hard-codes "an
   image is RGBA8 PNG". Colour space, bit depth and alpha travel with media.
6. **Never crash.** No `unwrap`/`expect`/`panic!`/indexing in non-test code
   (clippy-enforced), `unsafe_code = "forbid"`, hostile-input discipline for files, params and
   provider responses, a last-resort panic guard around command dispatch, and a `panic_hunt`
   test that fuzzes every command with adversarial params.
7. **Layering is enforced** by `cargo xtask layers`. Nothing below the UI layer uses egui,
   eframe, winit or rfd.
8. **Never break wasm.** Core crates must build for `wasm32-unknown-unknown`; filesystem and
   threads go through platform services.
9. **Pure Rust by default.** C/C++ dependencies only behind Cargo features in edge crates
   (e.g. FFmpeg for video).
10. **Clean-room.** Behaviour and look studied, never code or assets copied. Third-party assets
    are permissively licensed and listed in `ATTRIBUTION.md`.
11. **Performance is a feature.** 60 fps with 1,000 nodes on screen; thumbnails and decoded
    textures cached per content hash; heavy work off the UI thread.

---

## 4. Architecture

### 4.1 Workspace and layers

```text
crates/
  types        L0  ids, Modality, PortType, Value, Hash, Money, error base (pure data, serde)
  media        L0  standalone: sniffing, decode/encode, thumbnails, metadata (image; video/audio
                    probes behind features); no workspace deps
  comfy        L1  ComfyUI client and workflow model: servers, /object_info, API/UI workflow
                    import, port mapping, upload, prompt queue, WebSocket progress, outputs
  graph        L1  document model: Project, Canvas, Node, Edge, Field, Comment, Tag, Element,
                    Generation, TechniqueDef; Changeset (apply/invert/validate)
  nodes        L2  NodeKind trait + registry, built-in node descriptors and param schemas
  catalog      L2  provider + model descriptors, capability matching, pricing, auto-select rules
  actions      L2  deterministic image/video/text actions (CPU reference, rayon)
  exec         L3  planner (routers, batches, techniques → job DAG), scheduler, queue,
                    cancellation, staleness, cost quotes, budgets; async-runtime agnostic
  providers    L3  Provider trait + adapters (comfyui, comfy-cloud, fal, replicate,
                    openai-images, google [imagen, gemini-image, veo, tts], elevenlabs,
                    bytedance [seedream, seedance], kling, tripo, meshy,
                    llm [openai-compatible, anthropic, litellm]), each behind a feature
  gpu          L3  wgpu actions, custom WGSL actions, 3D preview renderer
  format       L3  .ncraft project files, content-addressed asset store, atomic saves, migrations
  engine       L4  Session, command registry, undo stack, event bus, settings, library
  agent        L5  in-canvas agent: LLM tool loop over engine commands
  automation   L5  control channel (JSON), MCP server, HTTP serve mode for techniques
  ui-egui      L5  egui shell: canvas widget, node bodies, editors, panels, theme tokens
  testkit          fixtures, mock provider, snapshot helpers
apps/
  nodecraft        desktop app (eframe/wgpu)
  nodecraft-cli    headless: run graph/technique, batch from CSV, inspect, mcp, serve
  nodecraft-web    the same app in the browser (trunk + wasm-bindgen), later
xtask/             layers | wasm | ci | catalog-check | snapshots
```

```mermaid
flowchart BT
  types --> graph
  media --> graph
  types --> comfy
  graph --> nodes & catalog & actions
  comfy --> nodes & providers
  nodes --> exec
  catalog --> exec & providers
  actions --> exec & gpu
  graph --> format
  exec & providers & gpu & format --> engine
  engine --> agent & automation & ui[ui-egui]
  agent & automation & ui --> app[apps/nodecraft]
  engine & automation --> cli[apps/nodecraft-cli]
```

### 4.2 Core types (sketch)

```rust
// types
pub enum Modality { Text, Image, Video, Audio, Mesh, Document }
pub struct PortType { pub modality: Modality, pub many: bool }        // many = list input
pub enum Value { Text(String), Media(AssetRef), List(Vec<Value>), /* … */ }
pub struct AssetRef { pub hash: Blake3, pub mime: Mime, pub meta: MediaMeta }

// nodes
pub trait NodeKind: Send + Sync {
    fn descriptor(&self) -> &NodeDescriptor;              // id, ports, ParamSchema, category
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId>; // e.g. text→image vs image→image
    fn plan(&self, ctx: &PlanCtx) -> Result<Vec<JobSpec>, PlanError>;
}
pub struct ParamSchema(Vec<ParamSpec>);  // slider, enum, toggle, color, text, model, seed…
                                         // with visible_if conditions and defaults

// graph
pub struct Node { pub id: NodeId, pub kind: KindId, pub title: String, pub pos: Pos2,
                  pub params: ParamValues, pub inputs: Vec<InputSlot>,   // ordered
                  pub history: Vec<GenerationId>, pub active: Option<GenerationId>,
                  pub tag: Option<Tag>, pub field: Option<FieldId> }
pub struct Generation { pub id: GenerationId, pub outputs: Vec<Value>, pub model: Option<ModelId>,
                        pub params: ParamValues, pub input_hash: Blake3, pub cost: Money,
                        pub status: GenStatus, pub created: Timestamp }
pub enum Change { AddNode(Node), RemoveNode(NodeId), Connect(Edge), SetParam { .. }, … }
pub struct Changeset { pub changes: Vec<Change> }   // apply() -> inverse Changeset

// catalog / providers
pub struct ModelDescriptor { pub id: ModelId, pub provider: ProviderId, pub modes: Vec<ModeSpec>,
                             pub params: ParamSchema, pub pricing: PricingRule,
                             pub latency_hint: Duration, pub quality: u8, pub summary: String }
#[async_trait] pub trait Provider: Send + Sync {
    async fn submit(&self, job: &Job, cx: &JobCx) -> Result<JobHandle, ProviderError>;
    async fn poll(&self, h: &JobHandle) -> Result<JobProgress, ProviderError>;
    async fn cancel(&self, h: &JobHandle) -> Result<(), ProviderError>;
}
```

### 4.3 Commands, changesets and events

```text
UI / CLI / MCP / agent ──► engine.dispatch(CommandId, params)
                                  │ validates, builds Changeset(s), may enqueue jobs
                                  ▼
                          graph.apply(changeset) ─► inverse pushed to undo stack
                                  │                 journal → autosave / sync
                                  ▼
                          EventBus: DocChanged, JobQueued, JobProgress, JobDone, JobFailed
                                  ▼
                          UI requests repaint; automation streams events
```

Job results come back as events and are applied as `AddGeneration` changesets, so history, undo,
autosave and future collaboration all see them the same way.

### 4.4 Execution engine (`exec`)

1. **Plan:** from the requested nodes (and their stale ancestors in "with upstream" mode), resolve
   routers to their sources, inline technique subgraphs, expand batches (cross/zip) into item
   jobs, check every input is ready, and price the plan → `RunPlan { jobs, edges, cost }`.
2. **Confirm:** UI shows count and cost; budgets can block.
3. **Schedule:** topological order with per-provider concurrency limits and a global limit;
   deterministic local actions on a rayon pool, network jobs on the async runtime.
4. **Run:** submit → poll/stream → download outputs into the asset store (hash-verified, size
   capped) → `JobDone`. Retries with backoff for transient errors, idempotency keys so a retry
   never double-charges.
5. **Cancel:** cooperative cancellation per job, per selection, or all.

The runtime is abstracted (`Spawner` trait): tokio on desktop, `wasm-bindgen-futures` in the
browser. HTTP via `reqwest` (works on both).

### 4.5 Media pipeline

- **Images:** `image` crate (+ `fast_image_resize`); thumbnails per content hash; GPU textures
  cached and evicted by LRU budget.
- **Video:** FFmpeg via a sidecar process (`ffmpeg-sidecar`-style) behind the `video` feature
  on desktop; decode frames for previews/scrubbing, encode for timeline renders and actions.
  Pure-Rust paths (e.g. `rav1e`/`re_mp4` style) evaluated later; browser uses WebCodecs.
- **Audio:** `symphonia` decode, `rodio`/`cpal` playback, waveform thumbnails.
- **3D:** `gltf` import/export (+ OBJ), wgpu viewer as an egui paint callback, offscreen render
  for photo/turntable/sheet capture.
- **PDF:** page rendering and text extraction through `craft-pdf` (pure Rust, from craft-libs;
  features `read`/`raster`), behind a `pdf` feature.
- **Text layout in Layer Editor:** `parley`/`cosmic-text` with user-uploaded fonts.

### 4.6 Storage

- **Project file `.ncraft`:** a zip (stored, not deflated, for media) with `project.json`
  (versioned schema, `#[serde(default)]` for forward compatibility) and `assets/<blake3>`;
  crash-safe saves through `craft-atomic` (same-directory staging, fsync) and an append-only
  changeset journal for crash recovery.
- **Global library** (app data dir): History index, Elements, Techniques, custom actions, model
  catalog cache. Settings in `config.toml` through `craft-settings` (platform folders, portable
  mode, recovery of a damaged file, migrations). Secrets in the OS keychain (`keyring`).
- **Large projects:** assets stay on disk and are loaded lazily; the document only holds refs.

### 4.7 UI (egui)

- **Own canvas widget** (in `ui-egui/canvas`): transform, culling, LOD (titles only when zoomed
  out), hit testing, noodle routing, selection, snapping. Existing crates (`egui-snarl`,
  `egui_node_graph2`) are references for interaction but too form-like for rich media cards, so
  we own this core piece.
- **Node body renderers** per kind with a generic fallback generated from the param schema.
- **Panels:** left rail (add, elements, history, techniques, assets, comments), right inspector
  (single or bulk), bottom queue, agent sidebar, full-screen editors (Layer, Timeline, Mask, 3D).
- **Theme tokens** (colours, radii, spacing, modality colours) in one place; dark default matching
  the references, light theme supported.
- **Shared Craft UI pieces:** `craft-ui` for SVG icon catalogs, icon buttons, tabs, pane controls,
  docking and theme helpers; `craft-quit` for platform-correct quit and close handling (macOS
  app menu, unsaved-work prompt hook). They are used only in `ui-egui` and the apps, so the
  layering rule holds.
- **UI state is serde data** so the control channel can read and drive it; visual tests render
  offscreen with `egui_kittest` and compare snapshots.

### 4.8 Automation surfaces

- **CLI** (`nodecraft-cli`, released for every desktop platform): `run <project|technique>
  --input k=v --out dir`, `batch --csv`, `quote` (cost only), `inspect`, `commands`, `providers`
  (list or test configured providers), `comfy` (servers, import a workflow), `mcp`, `serve`,
  `--version` with commit and build date.
- **Control channel:** local JSON over TCP/stdio: dispatch commands, read state, screenshot.
- **MCP servers (integrated, shipped in every release):**
  - **Headless:** `nodecraft-cli mcp` (stdio, and streamable HTTP with `--listen`) opens or
    creates a project in a session without a window. Filesystem access is confined to
    `--automation-read-root` / `--automation-write-root`, following PhotoCraft's MCP conventions.
  - **Bridge to the running app:** `nodecraft-cli mcp --bridge` attaches to a desktop app started
    with `--control` (token file), so an agent works on the canvas the user sees, live.
  - **Built into the desktop app:** Settings → Automation → "Enable MCP server" starts the same
    server inside the app on localhost with a generated token, and shows the client config to
    copy.
  - **Tools:** `command_list`, `command_run`, `command_batch`, `canvas_inspect`, `node_inspect`,
    `run_quote`, `run_nodes`, `job_status`, `render_preview`, `asset_import`, plus one tool per
    published technique (inputs from its exposed ports). Every tool has a title and annotation
    hints. Cost-incurring tools require a budget and return the quote first.
- **HTTP serve mode:** `POST /techniques/{id}/runs`, poll or webhook; for local or self-hosted use.

---

## 5. Repository setup (modelled on PhotoCraft)

```text
NodeCraft/
├─ Cargo.toml            [workspace] resolver 3, edition 2024, shared deps, lints, profiles
├─ rustfmt.toml          max_width = 160, use_small_heuristics = "Max"
├─ clippy.toml           allow unwrap/expect/panic/indexing in tests only
├─ .cargo/config.toml    alias xtask = "run -p xtask --"
├─ .config/nextest.toml  ci profile
├─ .github/workflows/    ci.yml (claim-bot tests; fmt, clippy -D warnings, tests on
│                        Linux/Windows/macOS, xtask layers/wasm once the workspace exists),
│                        claim.yml (/claim bot + daily sweep), release.yml (§5.1)
├─ packaging/            per-OS and web packaging scripts used by `.github/workflows/release.yml`
├─ .github/              pull_request_template.md, ISSUE_TEMPLATE/
├─ scripts/claim/        claim bot logic + node:test suite (adapted from SolidCraft)
├─ AGENTS.md             orientation for agents, pointing at CONTRIBUTING.md and docs/
├─ CONTRIBUTING.md       claiming issues, non-negotiable rules, tests, checklists
├─ README.md
├─ ROADMAP.md            one-page status
├─ docs/                 plan.md (this file), architecture.md, development.md, decisions/,
│                        ui-design.md, node-catalog.md, providers.md
├─ crates/ apps/ xtask/  as in §4.1
├─ assets/               icons (original, licensed), catalog data
├─ LICENSE-MIT, LICENSE-APACHE, NOTICE, ATTRIBUTION.md
└─ (gitignored) dev_assets/  plan/  log/  target/
```

Workspace-wide settings to copy:

```toml
[workspace.package]
edition = "2024"
license = "MIT OR Apache-2.0"
rust-version = "1.95"

[workspace.dependencies]
egui = { version = "0.36", default-features = false }   # the version craft-ui builds against
eframe = { version = "0.36", default-features = false }
# craft-libs, pinned by immutable rev (bump deliberately, one PR per bump)
craft-ui       = { git = "https://github.com/storytold/craft-libs", rev = "8f5a2831614e632f91ad367f7af2a6e5b61b9529", features = ["serde"] }
craft-settings = { git = "https://github.com/storytold/craft-libs", rev = "8f5a2831614e632f91ad367f7af2a6e5b61b9529" }
craft-atomic   = { git = "https://github.com/storytold/craft-libs", rev = "8f5a2831614e632f91ad367f7af2a6e5b61b9529" }
craft-quit     = { git = "https://github.com/storytold/craft-libs", rev = "8f5a2831614e632f91ad367f7af2a6e5b61b9529", features = ["menu"] }
craft-pdf      = { git = "https://github.com/storytold/craft-libs", rev = "8f5a2831614e632f91ad367f7af2a6e5b61b9529", features = ["raster"] }

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
dbg_macro = "warn"
todo = "warn"
# per crate: #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic,
#                    clippy::unimplemented, clippy::todo, clippy::unreachable)]

[profile.dev.package."*"]
opt-level = 2           # fast media decoding in debug builds
[profile.release]
lto = "thin"
codegen-units = 1
```

Testing strategy:

- **Unit + property tests** (`proptest`) for changeset apply/invert round-trips, planner
  (routers, batch cross/zip, technique inlining), staleness, pricing.
- **Golden graphs:** small `.ncraft` fixtures run against the **mock provider** (deterministic
  fake images/text) and compared with expected outputs.
- **`panic_hunt`:** every command with adversarial params must return `Err`, never panic.
- **UI snapshots** with `egui_kittest` (canvas, node cards, editors) checked by eye before
  committing.
- **Provider contract tests** recorded against fixtures; live tests opt-in behind env vars.

### 5.1 Releases and distribution

All builds are distributed as **GitHub releases**. `.github/workflows/release.yml` follows the
shared Craft release workflow (`storytold/craft-common-actions`) but is NodeCraft's own, so that
releases don't depend on signing secrets (docs/decisions/2026-10-16-m0-skeleton.md). It runs the per-app `packaging/`
scripts (`packaging/env.sh`, `packaging/{macos,windows,linux}/…`, modelled on PhotoCraft's).

- **Flow:** bump `[workspace.package] version` with `cargo xtask version set X.Y.Z`, merge
  `main` into the `release` branch, and the workflow builds a **draft** release `NodeCraft vX.Y.Z`
  with every artifact, `SHA256SUMS.txt` and notes generated from merged PRs. Check it, then
  publish (which creates the tag). `-rc.N` versions are marked as pre-releases.
- **Artifacts:**

  | Platform | Desktop app | CLI (includes the MCP server) |
  |---|---|---|
  | macOS (universal) | `nodecraft-<v>-macos-universal.dmg` | `nodecraft-cli-<v>-macos-universal.zip` |
  | Windows x64, arm64 | `.msi` + portable `.zip` | in the same packages |
  | Linux x86_64 / aarch64 | `.AppImage`, `.deb`, `.rpm`, `.tar.gz`, Flatpak | in the same packages |
  | Web | `nodecraft-web-<v>.zip`: static site from `apps/nodecraft-web` (trunk + wasm-bindgen, size-optimised `wasm-release` profile, split into parts under host size caps) | n/a |

- **Browser build specifics:** providers are called directly from the browser where their CORS
  policy allows it, otherwise through a user-configured LiteLLM or ComfyUI endpoint. Keys live in
  browser storage for that origin only, and the UI says so. Projects open and save via the File
  System Access API, with download/upload as the fallback.
- **Signing:** optional. With the secrets set, macOS builds are notarised and Windows builds
  signed; without them the release still builds (ad-hoc/unsigned, with a `::warning::`) and its
  notes link `docs/install.md`, which walks users past Gatekeeper and SmartScreen.
- **Version info:** `nodecraft --version`, `nodecraft-cli --version` and *Help › About* report
  version, commit and build date (`NODECRAFT_BUILD_SHA`, `NODECRAFT_BUILD_DATE`).

---

## 6. Key decisions and trade-offs

| Decision | Choice | Why |
|---|---|---|
| UI toolkit | egui/eframe on wgpu | Native, cross-platform, same code to wasm, immediate mode suits a live canvas |
| Node canvas | Own widget | Rich media cards, prompt overlays, editors-in-node and LOD exceed form-style node libs |
| Run semantics | Explicit runs for generative nodes, live for deterministic ones, stale badges | Generations cost money and are non-deterministic; actions are free and should feel instant |
| Model integration | Catalog as data + thin provider adapters | Models change monthly; adding one must not need a release |
| Providers | ComfyUI (first-class, incl. 3D) + Comfy Cloud, fal.ai + Replicate, OpenAI Images, Google Imagen/Gemini image/Veo/TTS, ElevenLabs, ByteDance Seedream + Seedance, Kling, Tripo AI, Meshy, OpenAI-compatible/Anthropic/LiteLLM LLM endpoints; more as `future` issues | Local-first via ComfyUI, the leading vendors directly, and aggregators/LiteLLM for breadth |
| Licence | MIT OR Apache-2.0 | Same as the Craft suite and craft-libs |
| Shared libraries | craft-libs (`craft-ui`, `craft-settings`, `craft-atomic`, `craft-quit`, `craft-pdf`), pinned by rev | Shared look and behaviour with the Craft apps; fixes land upstream once |
| Multi-user work | `/claim` bot + draft-PR claims (CONTRIBUTING.md) | Lets several people and agents pick issues without collisions |
| Video | FFmpeg sidecar behind a feature | Only realistic route to broad codec support today; keeps core pure Rust |
| Custom actions | LLM-written WGSL with declared params | Sandboxed by construction, GPU fast, parameters become sliders |
| Persistence | Local `.ncraft` + content-addressed assets | Offline, portable, dedupes variations; cloud sync is an add-on |
| Distribution | GitHub releases from our own workflow modelled on the shared Craft one: desktop installers, CLI with integrated MCP server, wasm web bundle | Same artifact names as the other Craft apps; releases don't wait for signing |
| Collaboration | Single-user first; changesets now, CRDT sync in M10 | Zero cost now, avoids a rewrite later |

Still open:

1. **Signing certificates:** Apple Developer ID and a Windows code-signing route (#100). Until
   they exist, releases ship unsigned with first-launch instructions (docs/install.md).

---

## 7. Roadmap

Each milestone ends with a green tree (fmt, clippy, tests, layers, wasm check) and a demo.

| M | Name | Contents | Exit criterion |
|---|---|---|---|
| 0 | **Skeleton** | Workspace, lints, xtask layers/wasm, CI, AGENTS.md, empty crates, eframe window | `cargo run` opens a themed empty canvas on all three OSes |
| 1 | **Canvas core** | Graph model + changesets + undo, command registry, canvas widget (pan/zoom/select/noodles/minimap), source nodes via drag-drop/paste, "Turn into" picker, shortcuts, `.ncraft` save/load | Build and save a 200-node graph at 60 fps; undo everything |
| 2 | **Organise** | Fields, tags, comments, search, auto-format, bulk panel, router node, input ordering | Workshop "organise" patterns work |
| 3 | **Generation** | `exec` planner/scheduler, queue, status, history strip, variations, cost quotes, budgets, catalog, mock provider, keychain; **ComfyUI** (servers panel, workflow node, WebSocket progress), **LLM endpoints** (OpenAI-compatible, Anthropic, LiteLLM), **fal.ai**, **Replicate**, **OpenAI Images**, **Google Imagen + Gemini image**, **Seedream** | Combine → branch → prompt-helper workflows run end to end, both on cloud models and fully offline on ComfyUI + Ollama |
| 4 | **Actions** | Image + text actions (CPU), live recompute, split into grid, GPU path, custom WGSL actions | Colour grade/blur/crop chains preview live |
| 5 | **Batch & Techniques** | Batch node (cross/zip, CSV, split), Technique Builder, technique node, app mode, library, templates; CLI `run`/`batch`/`quote`; **Comfy Cloud**; first GitHub release (desktop + CLI) | Publish a technique and run it from canvas, app mode and CLI with identical results |
| 6 | **Media editors** | Layer Editor (layers, transform, text, shapes, fonts), Mask/Inpaint, remove background/upscale tools, Elements | Composite → re-render and Element workflows |
| 7 | **Video & audio** | Video/audio nodes, playback, FFmpeg sidecar, video actions, Timeline editor; **Veo**, **Seedance**, **Kling**, **ElevenLabs**, **Google TTS** adapters; ComfyUI video/audio workflows | Four clips stitched with fades and audio, rendered |
| 8 | **3D & documents** | 3D node + viewer, photo/turntable/sheet capture, GLB/OBJ export; **Tripo AI**, **Meshy**, ComfyUI 3D workflows; PDF node | Image → 3D → sheet → Element |
| 9 | **Agent & automation** | Agent sidebar (assist/auto), control channel, MCP servers (headless, bridge, in-app; commands + techniques), `serve` | Agent builds and runs a 5-node workflow from one sentence within budget |
| 10 | **Share & collaborate** | Browser release polish (the wasm bundle ships from M5 as a preview), CRDT sync, presence, shared comments, share links, decks | Two users edit one canvas live |

---

## 8. First steps (M0 checklist)

- [x] `Cargo.toml` workspace with the settings in §5; `rustfmt.toml`, `clippy.toml`,
      `.cargo/config.toml`, `.config/nextest.toml`
- [x] Crates from §4.1 as empty libraries with the deny-panic header; apps `nodecraft` and
      `nodecraft-cli`
- [x] `xtask layers` (table of crate → layer, fails on violations) and `xtask wasm`
- [x] `ci.yml` (claim-bot tests now; Rust jobs switch on when `Cargo.toml` lands), `claim.yml`,
      PR and issue templates, `CONTRIBUTING.md`, licences
- [ ] Switch CI to nextest once there are enough test binaries to matter
- [x] craft-libs dependencies pinned in `[workspace.dependencies]` (§5)
- [x] `AGENTS.md`, `docs/architecture.md` (from §4), `docs/decisions.md`, `ROADMAP.md`
- [x] `release.yml` calling the shared Craft release workflow, `packaging/` scripts, `xtask version`
- [x] GitHub labels: `area:*` (CONTRIBUTING.md), `in progress` (created by the bot on first use),
      `good first issue`; one issue per M0/M1 task so contributors can `/claim` them
- [x] `ui-design.md` with theme tokens taken from the reference look (dark, dot grid, radii)
- [x] eframe app with the dot-grid canvas, pan/zoom, and a stub node card
