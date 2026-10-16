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
  <img alt="Status: M0 skeleton" src="https://img.shields.io/badge/status-M0%20skeleton-d69e2e?style=flat-square">
</p>

---

> [!NOTE]
> NodeCraft is at milestone M0: the workspace, CI and release pipeline exist, and the app opens
> an empty themed canvas. The full plan lives in [`docs/plan.md`](docs/plan.md), the status in
> [`ROADMAP.md`](ROADMAP.md).

## What it will be

- **Infinite canvas.** Nodes for text, image, video, audio, 3D and PDFs, joined by noodles.
  Combine sources, branch three ways from any result, and keep the whole creative path visible
  from left to right.
- **Any model, one node per modality.** The model is a parameter: search for compatible models,
  swap without rewiring, auto-select, or run several models side by side. Each run shows its cost
  before it starts.
- **ComfyUI first-class.** Import any ComfyUI workflow as a node with typed ports, follow live
  progress, and switch Image/Video nodes between cloud models and local ComfyUI without
  rewiring. It covers image, video, audio and 3D workflows, locally or on Comfy Cloud.
- **Providers:** fal.ai and Replicate; OpenAI Images; Google Imagen, Gemini image, Veo and TTS;
  ElevenLabs; ByteDance Seedream and Seedance; Kling; Tripo AI and Meshy for 3D; and any
  OpenAI-compatible, Anthropic or LiteLLM endpoint. More are tracked as `future` issues.
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
- Shares UI, settings, atomic-save, quit and PDF libraries with the other Craft apps through
  [craft-libs](https://github.com/storytold/craft-libs).

See [`docs/plan.md`](docs/plan.md) for the feature catalogue, architecture, repository
conventions and roadmap.

## Build from source

```sh
cargo run -p nodecraft          # the desktop app
cargo xtask ci                  # fmt, clippy, tests, layering and wasm checks
```

[`docs/development.md`](docs/development.md) has the details; [`AGENTS.md`](AGENTS.md) is the
orientation for contributors and coding agents.

## Downloads

Releases are published on [GitHub Releases](../../releases). Each release includes desktop
installers for macOS, Windows and Linux; `nodecraft-cli`, with the integrated MCP server
(`nodecraft-cli mcp`); and a WebAssembly build that runs in the browser. Nothing has been
released yet.

## Contributing

Pick an issue and comment `/claim`. A bot assigns it to you, and you open a draft PR within
72 hours. [`CONTRIBUTING.md`](CONTRIBUTING.md) has the claim rules, the non-negotiable code rules
and checklists for adding commands, node kinds and providers.

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

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
