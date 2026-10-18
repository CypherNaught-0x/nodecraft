# Roadmap

**Now: the wiring milestones.** M0 is done and M1 is nearly done; the generation engine (M3) and
the action library (M4) are built and heavily tested, and the current work is connecting them to
the app. Details and exit criteria per milestone: [`docs/plan.md`](docs/plan.md) §7. Work is
tracked as GitHub issues in milestones; comment `/claim` to take one.

Status is measured from the issues in each milestone as of 2026-10-18: 228 filed, 127 open, 99
closed as completed and 2 closed as not planned. **Done** counts issues closed as *completed*, so
M1 reads 19/28 where GitHub says 20 closed (#135 was declined rather than built). Status describes
shipped behaviour, not code that merely exists: a milestone only counts what a person or an agent
can actually do today.

| M | Name | Status | Done |
|---|---|---|---|
| 0 | **Skeleton** — workspace, lints, xtask, CI, release pipeline, themed canvas | ✅ done | 10/10 |
| 1 | **Canvas core** — graph + changesets + undo, commands, canvas widget, `.ncraft` | 🟡 nearly — see caveat | 19/28 |
| 2 | **Organise** — fields, tags, comments, search, auto-format, router, input order | 🟡 partial | 2/8 |
| 3 | **Generation** — exec, queue, cost, catalog, ComfyUI, LLMs, cloud providers | 🟡 engine done, app not wired | 14/27 |
| 4 | **Actions** — image/text actions, GPU path, live recompute, custom WGSL | 🟡 library done, no action nodes | 4/10 |
| 5 | **Batch & Techniques** — batch, Technique Builder, app mode, library, CLI `run`, release | ⬜ not started | 0/15 |
| 6 | **Media editors** — Layer Editor, Mask/Inpaint, Elements | ⬜ not started | 0/5 |
| 7 | **Video & audio** — nodes, playback, Timeline, FFmpeg, audio/video providers | 🟡 sidecar only | 1/13 |
| 8 | **3D & documents** — 3D viewer, capture, GLB/OBJ, PDF, 3D providers | ⬜ not started | 0/6 |
| 9 | **Agent & automation** — MCP, control channel, agent sidebar, serve | 🟡 surfaces done, no agent | 1/5 |
| 10 | **Share & collaborate** — browser polish, CRDT sync, presence, decks | ⬜ not started | 0/5 |
| — | **Future providers** — the 25 further adapters tracked as `future` | ⬜ none started | 0/25 |

## What works today

Run it: `cargo run -p nodecraft`. What the window actually does:

- **Canvas**: pan/zoom, dot grid, graph-backed node cards whose controls are generated from the
  node's parameter schema, noodles (draw, rewire, delete), selection by click/Shift/marquee and
  upstream/downstream/kind, minimap with zoom-to-fit and zoom-to-selection (Shift+1/Shift+2/Shift+M), incremental
  search (⌘F), double-click quick-add palette with per-key shortcuts, "Turn into" on
  drop-to-empty, input-order badges with drag-to-reorder, copy/cut/paste/duplicate through the OS
  clipboard, OS file drop as source nodes, per-node generation history strip, an API keys panel
  backed by the OS keychain.
- **Document integrity**: every edit is an invertible changeset; undo/redo with a bounded stack;
  command dispatch under a panic guard; 41 registered commands with typed JSON params.
- **Automation**: `nodecraft-cli mcp` (MCP over stdio, 4 generic tools), `mcp --bridge` to drive a
  running app, `nodecraft-cli control`, and `nodecraft --control` (local JSON control channel with
  screenshots). See [`docs/mcp.md`](docs/mcp.md) and [`docs/control.md`](docs/control.md).
- **Headless**: the planner, scheduler, driver, run executor, cost quotes, budgets, usage ledger,
  staleness hashing and router resolution exist and are tested end to end against the mock
  provider.

## What does not work yet

The honest list, because "it's in the repo" and "you can use it" are different claims:

- **The app cannot generate.** Nothing constructs a provider adapter or supplies the run host
  resources, so `run.quote`/`run.start` report *unsupported*, there is no Run button, and no
  `Category::Generation` node kind is registered outside the testkit. Four adapters (LLM
  endpoints, OpenAI Images, Replicate, ComfyUI) work as libraries with contract tests, and the
  catalog carries 6 models across those three provider families.
- **The app cannot save or open.** `.ncraft` archives, content-addressed assets, atomic writes and
  crash recovery all exist, but no host configures project storage for a session, so `file.*`
  reports *unsupported*. There is also no file picker yet.
- **Actions do not run.** 13 image and 4 text operations exist in CPU and WGSL form, but no action
  node kind reaches them and cards do not render image pixels yet, so nothing recomputes live.
- **The in-canvas agent is a 5-line stub.** `crates/agent` has no code; the MCP server and control
  channel are what external agents use today.
- **No browser build.** Every library crate compiles to wasm and `web` is reserved in the layer
  map, but there is no `apps/nodecraft-web`. Dropping files in a browser is an explicit
  *unsupported*.
- **FFmpeg is CI-only.** The video sidecar exists behind a `video` feature that no product crate
  enables yet; there are no video or audio nodes, and no video actions.
- **Not started**: fields/tags/comments UI (the data model and changes exist, nothing edits them),
  batch, Techniques, app mode, library, Layer/Timeline/Mask editors, Elements, 3D, PDF,
  collaboration, auto-updates, and 25 of the advertised providers (fal.ai, Google, ElevenLabs,
  Seedream/Seedance, Kling, Tripo, Meshy and the `future` list).

## Next up

The open pull requests say what is in flight; the near-term order that makes the engine usable is:

1. **Wire the app to storage** — session storage plus a file picker, so save, open and crash
   recovery work in `cargo run -p nodecraft` (#147 lazy assets is open, follow-ups #334/#335).
2. **Wire a run host** — construct adapters from the catalog, register the `run.start` service and
   the host resources, and put a Run button on the card (#211, #201, #275, #157).
3. **First generation node kinds** — image/text generation kinds so there is something to run
   (#83, #84), then stale badges with one-click refresh (#211).
4. **Action nodes** — expose the CPU/GPU operations as nodes with live recompute (#88, #90) and
   render image previews on cards.
5. **Organise** — fields (#66), tags (#67), comments and Note nodes (#68), auto-format (#70),
   persistence versioning for them (#193).
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
