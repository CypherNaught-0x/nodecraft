<h1 align="center">NodeCraft</h1>

<p align="center">
  <b>A native, node-based canvas for generative media, written in pure Rust.</b><br>
  Wire text, image, video, audio, 3D and documents into any AI model and deterministic tools,<br>
  branch ideas visually, and package workflows as reusable Techniques.
</p>

<p align="center">
  <img alt="100% Rust" src="https://img.shields.io/badge/100%25-Rust-b7410e?style=flat-square&logo=rust">
  <img alt="macOS · Windows · Linux" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-native-2f7bf5?style=flat-square">
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-3a3a3a?style=flat-square">
  <img alt="Status: M1 finishing, M3 wiring" src="https://img.shields.io/badge/status-M1%20finishing%20%C2%B7%20M3%20wiring-d69e2e?style=flat-square">
</p>

---

> [!NOTE]
> NodeCraft is being built in the open, three days in. The canvas, the document model, undo,
> automation surfaces and the whole generation engine are in the tree; the step in progress is
> wiring that engine to the app, so **the app does not generate or save files yet**.
> [`ROADMAP.md`](ROADMAP.md) says exactly what works, what does not, and what is next.

## What it does today

`cargo run -p nodecraft` opens a real editing canvas:

- **Edit**: pan/zoom dot-grid canvas, node cards generated from each node's parameter schema,
  noodles you can draw, rewire and delete, selection by click/Shift/marquee plus
  upstream/downstream/by-kind, a minimap with zoom-to-fit and zoom-to-selection, incremental search,
  a searchable quick-add palette with per-key shortcuts, "Turn into" when you drop a noodle on
  empty canvas, drag-to-reorder input order badges, copy/cut/paste/duplicate through the OS
  clipboard, and OS file drop that creates source nodes.
- **Never lose work**: every edit is an invertible changeset, so undo/redo covers everything, and
  command dispatch runs under a panic guard. No `unsafe` and no `unwrap` outside tests.
- **Keep your keys yours**: an API keys panel over the OS keychain.
- **Drive it from outside**: an MCP server (`nodecraft-cli mcp`, and `--bridge` to drive the app
  you have open), a headless control channel (`nodecraft --control`, `nodecraft-cli control`), all
  dispatching the same 41 commands the UI uses.

## What it will do

The engine behind generation is already built and tested — plan → quote → budget → schedule →
execute → record → history, with cost shown before a run and budgets held while jobs run. The work
now is connecting it to the window. The target:

- **Any model, one node per modality.** The model is a parameter: search compatible models, swap
  without rewiring, or run several side by side. Each run shows its cost before it starts.
- **ComfyUI first-class.** Import any workflow as a node with typed ports, follow live progress,
  and switch a node between cloud models and local ComfyUI without rewiring.
- **Providers**: LLM endpoints (OpenAI-compatible, Anthropic, LiteLLM, Ollama), OpenAI Images and
  Replicate are written; fal.ai, Google Imagen/Gemini image/Veo/TTS, ElevenLabs, Seedream and
  Seedance, Kling, Tripo and Meshy are tracked as issues.
- **Deterministic actions** — colour grading, blur, crop, aspect ratio, split into grid, text and
  video operations — recompute live, with custom actions authored as GPU shaders.
- **Editors inside nodes**: a Layer Editor, a Timeline, Mask/Inpaint, and a 3D viewer that captures
  photos, turntables and multi-view sheets.
- **Reuse**: Batch and Router nodes, Elements, and a Technique Builder that turns a workflow into
  one node, an app-mode form, a CLI command and an MCP tool.
- **Agent-ready**: an in-canvas agent builds and runs workflows with the same commands as the UI.
- **Yours**: local `.ncraft` projects with content-addressed assets, working offline with your own
  keys.

## Under the hood

- A Rust 2024 workspace of 17 layered crates and 2 apps (`types` → `graph` →
  `nodes`/`catalog`/`actions` → `exec`/`providers`/`gpu`/`format` → `engine` →
  `ui-egui`/`agent`/`automation`), with the layering enforced by `cargo xtask layers`.
- egui/eframe on wgpu. Every library crate also builds for `wasm32-unknown-unknown`, checked by
  `cargo xtask wasm`, ready for a browser version.
- Every action is a command with typed JSON params, and every edit is an invertible changeset —
  undo, autosave, automation and future live collaboration all use that one path.
- Built never to crash: no `unsafe`, no `unwrap`/`panic` outside tests, hostile input bounded at
  every boundary, and `panic_hunt` fuzzing each command with adversarial parameters.
- 1,200+ tests and rising, including property tests for changesets and the planner, golden
  graphs against a mock provider, fixture contract tests for providers, and `egui_kittest` tests
  that push real pointer and key events through the canvas.
- Shares UI, settings, atomic-save, quit and PDF libraries with the other Craft apps through
  [craft-libs](https://github.com/storytold/craft-libs).
- A dated decision record per choice in [`docs/decisions/`](docs/decisions/) explains the
  trade-offs a later contributor might otherwise undo (`cargo xtask decisions` prints the count).

See [`docs/plan.md`](docs/plan.md) for the feature catalogue, architecture and roadmap, and
[`docs/architecture.md`](docs/architecture.md) for the crate map.

## Build from source

```sh
cargo run -p nodecraft          # the desktop app
cargo xtask ci                  # fmt, clippy, tests, layers, decisions, wasm
```

[`docs/development.md`](docs/development.md) has the details; [`AGENTS.md`](AGENTS.md) is the
orientation for contributors and coding agents.

## Downloads

Two pre-releases exist. [`v0.2.0-rc.1`](../../releases/tag/v0.2.0-rc.1) is the current build:
installers for macOS, Windows and Linux of the canvas described above — try it, but know it still
cannot generate or save from the app. [`v0.1.0-rc.1`](../../releases/tag/v0.1.0-rc.1) is the
M0-era build that proved the release pipeline. Signed installers arrive with the first real
release ([#100](../../issues/100)); macOS and Windows builds are unsigned for now, so the first
launch shows a warning — [`docs/install.md`](docs/install.md) shows how to open them anyway.
Release builds ship `nodecraft-cli` with the integrated MCP server, bundled in the Linux and
Windows packages and also as a separate macOS download.

## Contributing

Pick an issue and comment `/claim`. A bot assigns it to you, and you open a draft PR within
72 hours. [`CONTRIBUTING.md`](CONTRIBUTING.md) has the claim rules, the non-negotiable code rules
and checklists for adding commands, node kinds and providers.

## Status and roadmap

| Milestone | Focus | Where it stands |
|---|---|---|
| M0 | Workspace skeleton, lints, CI, empty canvas | done |
| M1–M2 | Canvas core, organisation tools | canvas nearly done; save/open and fields/tags to wire |
| M3 | Generation engine, providers, history, cost | engine built and tested, app not wired yet |
| M4–M5 | Actions, Batch, Techniques, CLI | actions built as a library; M5 not started |
| M6–M8 | Layer/Timeline/Mask editors, video, audio, 3D, PDF | not started |
| M9–M10 | Agent, MCP, browser build, collaboration | MCP and control channel work; no agent yet |

Full detail, including what does not work yet: [`ROADMAP.md`](ROADMAP.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
