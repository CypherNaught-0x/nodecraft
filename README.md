<h1 align="center">NodeCraft</h1>

<p align="center">
  <b>A native, node-based canvas for generative media, written in pure Rust.</b><br>
  Wire text, image, video, audio, 3D and documents into any AI model and deterministic tools,<br>
  branch ideas visually, and package workflows as reusable Techniques.
</p>

<p align="center">
  <img alt="100% Rust" src="https://img.shields.io/badge/100%25-Rust-b7410e?style=flat-square&logo=rust">
  <img alt="macOS · Windows · Linux" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-native-2f7bf5?style=flat-square">
  <img alt="Status: planning" src="https://img.shields.io/badge/status-planning-d69e2e?style=flat-square">
</p>

---

> [!NOTE]
> NodeCraft is at the planning stage. No code has landed yet; the full plan lives in
> [`docs/plan.md`](docs/plan.md).

## What it will be

- **Infinite canvas.** Nodes for text, image, video, audio, 3D and PDFs, joined by noodles.
  Combine sources, branch three ways from any result, and keep the whole creative path visible
  from left to right.
- **Any model, one node per modality.** The model is a parameter: search for compatible models,
  swap without rewiring, auto-select, or run several models side by side. Each run shows its cost
  before it starts. Cloud providers and local backends (ComfyUI, Ollama) both work.
- **Explicit, cost-aware runs.** Runs go in dependency order, with variations, per-node
  generation history, stale badges when an upstream result changes, a queue, and budgets.
- **Deterministic actions.** Colour grading, blur, crop, aspect ratio, split into grid, and video
  and text operations recompute live. Custom actions are written for you as GPU shaders with
  generated sliders.
- **Editors inside nodes.** A Layer Editor for compositing with type and shapes, a Timeline for
  video, Mask/Inpaint, and a 3D viewer that captures photos, turntables and multi-view sheets.
- **Reuse.** Batch and Router nodes, Elements (consistent subjects), and the Technique Builder,
  which turns a workflow into one node, an app-mode form, a CLI command and an MCP tool.
- **Agent-ready.** An in-canvas agent builds and runs workflows through the same commands as the
  UI. A CLI, a control channel and an MCP server drive everything headless.
- **Yours.** Local `.ncraft` project files with content-addressed assets. Works offline, with
  your own keys kept in the OS keychain.

## Under the hood

- Rust 2024 workspace of layered crates (`types` → `graph` → `nodes`/`catalog`/`actions` →
  `exec`/`providers`/`gpu`/`format` → `engine` → `ui-egui`/`agent`/`automation`). The layering
  is enforced by `cargo xtask layers`.
- egui/eframe on wgpu for the desktop app. The core crates also build for WebAssembly, ready
  for a browser version.
- Every action is a command, and every edit is an invertible changeset. Undo, autosave,
  automation and future live collaboration all use that one path.
- Built never to crash: no `unsafe`, and no `unwrap`/`panic` outside tests. Every command is
  fuzz-tested with hostile parameters.

See [`docs/plan.md`](docs/plan.md) for the feature catalogue, architecture, repository
conventions and roadmap.

## Status and roadmap

| Milestone | Focus |
|---|---|
| M0 | Workspace skeleton, lints, CI, empty canvas |
| M1–M2 | Canvas core, organisation tools |
| M3 | Generation engine, providers, history, cost |
| M4–M5 | Actions, Batch, Techniques, CLI |
| M6–M8 | Layer/Timeline/Mask editors, video, audio, 3D, PDF |
| M9–M10 | Agent, MCP, browser build, collaboration |

## License

To be decided. The current plan is MIT OR Apache-2.0.
