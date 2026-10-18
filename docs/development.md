# Development

## Prerequisites

- Rust stable ≥ 1.95 (`rustup toolchain install stable`) with `rustfmt`, `clippy` and the wasm
  target: `rustup target add wasm32-unknown-unknown`.
- Linux: the usual eframe/wgpu system libraries (X11/Wayland, xkbcommon, Vulkan or GL drivers).
- Optional: [cargo-nextest](https://nexte.st) for faster test runs (`.config/nextest.toml`).

## Build and run

```sh
cargo run -p nodecraft              # the desktop app
cargo run -p nodecraft -- --version
cargo run -p nodecraft-cli -- --help
```

## Test

```sh
cargo xtask ci                       # the full gate: fmt, clippy -D warnings, tests, layers, wasm
cargo test -p nodecraft-ui-egui      # one crate
cargo nextest run --workspace        # optional, parallel across test binaries
node --test scripts/claim/*.test.js  # the claim bot
```

UI behaviour is tested headless with `egui_kittest` (see `crates/ui-egui/src/app.rs`): the tests
drive pointer and key events through the real canvas and check the resulting view and card state.

GitHub CI runs `cargo nextest run --workspace --profile ci` and then `cargo test --doc --workspace`
on Linux, Windows and macOS. The workflow pins nextest and verifies each release archive against
a checked-in SHA-256 before extracting it. When updating nextest, update the version and all three
checksums together from the `.sha256` files published beside the archives on the
[official nextest release](https://github.com/nextest-rs/nextest/releases). `cargo xtask ci` continues to use `cargo test`
so local checks do not require nextest.

## Benchmarks

- `cargo test -p nodecraft-ui-egui --release perf -- --ignored --nocapture`: canvas frame time over
  200- and 1,000-node graphs (see `docs/ui-design.md`, "Level of detail and performance").

## Performance capture

The app can time itself on demand (#151). Nothing is recorded until a capture starts, nothing
leaves the machine, and the report holds only numbers, metric names and build and platform
labels, never document contents or paths.

1. Start the app with the project and window size you want to measure, and wait for it to
   settle.
2. Run **Start performance capture** from the quick-add palette (double-click empty canvas,
   type "perf"), or call `perf.start` from the CLI, MCP or the control channel.
3. Optionally run **Clear performance capture** after warming up: every sample since the last
   clear counts.
4. Do the work being measured (pan, zoom, edit, undo, save).
5. Read the report with `perf.report`. From the palette, **Performance report** copies it to the
   clipboard. Stop with **Stop performance capture**. The headless CLI session (`control`, `mcp`)
   records too.

The report (`schema: "nodecraft.perf/2"`) gives, per metric, p50/p95/p99/max in microseconds,
the samples kept (the latest 4,096), recorded/invalid counts and an explicit availability reason.
Metrics:

- `frame_prepare`: shell CPU preparation, including workload collection;
- `frame_interval`: time between frame starts;
- `dispatch`: successful commands, including handlers and durable I/O;
- `graph_stage` and `graph_stage_refused`: successful and refused `Changeset::stage` calls,
  respectively, including draft cloning, applying changes, validation and inverse construction;
- `undo`, `redo`, `load` and `save`: overlapping classifications of successful dispatch;
- `presentation`: unsupported until a host supplies real presentation timestamps. It always has
  zero samples, null percentiles and `presentation_timestamps_not_available`, even with a CPU clock.

Stage samples exclude handler work, later replay/history/asset admission, durable append and final
publication. Successful staging followed by a later refusal still counts as executed staging;
preview/availability checks and nonmutating effects do not. Refusals are not mixed with successful
stage or dispatch latencies. These are overlapping boundaries, not additive phases.

The frame metrics count samples over 16,667 µs preparation and 1.5 times that between starts.
The app redraws on input: intervals over 250 ms are filtered as idle. These are interval-gap
heuristics, not observed dropped/presented frames. CPU preparation cannot prove 60 fps.

`workload` is the paint snapshot's visible/total nodes and edges, before later frame intents.
`visible_edges` uses the submitted cubic's adaptive flattening tolerance, actual painter clip,
and widest selected stroke/halo. The open-stroke envelope includes joins and antialiasing; it is
conservative near clip corners, not pixel-exact or an occlusion test. Both cards can be offscreen
while their curve crosses the clip. Neither endpoints nor the control hull alone decide visibility.
Invalid geometry/options gives `visible_edges: null` and a static `visibility_unavailable` reason,
not a plausible zero. Capturing adds a streaming flatten pass proportional to renderer work with
constant local geometry state; it allocates no per-edge point buffer or second tessellated mesh.
Disabled capture bypasses that pass. Rendering remains unchanged.

`environment` adds CPU count and the host's renderer label. Without a CPU clock (currently wasm),
CPU metrics are unsupported; previously retained samples can survive a subsequently failed clock.
Reports contain no document data and are never uploaded. Timing buffers stay bounded; this is not
a global memory/RSS guarantee.

The ignored `recorder_push_microbenchmark_on_and_off` measures only bounded recorder pushes,
excluding clock callbacks, dispatch and workload collection. It is not end-to-end capture cost.
For complete collection overhead, run the three bounded native paired benchmark tests once:

```sh
cargo test -p nodecraft-engine -p nodecraft-ui-egui --lib perf371_ -- --ignored --nocapture
```

They retain ordered off/on/on/off observations, include clock callbacks and all capture work,
and use fixed public fixtures with warmup excluded. UI timings cover actual show and equivalent
CPU tessellation on both sides, including adaptive visibility collection; no GPU, presentation,
window or assistive-technology-free production speed is inferred. Output includes workload,
callback/curve counts, build/platform and elapsed totals; no flaky timing threshold runs in CI.
Results depend on the machine, build and sampling order. Report the actual run and provenance
rather than reusing an old recorder-only nanosecond estimate.

## xtask

| Command | Does |
|---|---|
| `cargo xtask layers` | print the crate → layer table and fail on layering violations |
| `cargo xtask wasm` | `cargo check --target wasm32-unknown-unknown` for every layered crate |
| `cargo xtask catalog-check` | validate `assets/catalog/*.toml` and that each file is built in (`docs/catalog.md`); also part of `cargo test` |
| `cargo xtask decisions` | check `docs/decisions/`: dated record files with matching headings, and no entries in `docs/decisions.md`; also part of the `layers` step of `ci` |
| `cargo xtask ci` | all of the above plus fmt, clippy and tests; stops at the first failure |
| `cargo xtask version [set X.Y.Z]` | print or set the workspace version (Cargo.toml + Cargo.lock) |

`xtask` resolves the invoking workspace at runtime with Cargo's
[`locate-project --workspace`](https://doc.rust-lang.org/cargo/commands/cargo-locate-project.html),
including invocation from nested member directories. It validates workspace metadata with
`--locked --offline` and requires the `xtask` workspace member at `xtask/Cargo.toml` before
running a task. This read-only metadata preflight does not create or refresh a lockfile;
subsequent explicit tasks retain their normal Cargo lockfile behavior. The selected canonical root is printed to stderr as `xtask workspace: …`; `version` stdout remains
parseable. All child checks and version lockfile updates use that same validated root.

Neither compiled-in nor runtime `CARGO_MANIFEST_DIR` selects a checkout: Cargo documents it as
[package compilation/execution metadata](https://doc.rust-lang.org/cargo/reference/environment-variables.html),
which may refer to the binary's original package when a target cache is shared. A built xtask
can therefore be reused from another compatible workspace without checking or modifying the old
one. Invoke from inside the intended workspace (`cd` there first); invoking its binary from an
unrelated/outside directory fails instead of falling back. Selecting a workspace with an outer
`cargo --manifest-path` while remaining outside it is not an xtask root override.

## Build provenance

Set `NODECRAFT_BUILD_SHA` and `NODECRAFT_BUILD_DATE` at build time to stamp `--version` and the
About box; the packaging scripts do this (`packaging/env.sh`). Without them a build reports
`(dev build)`.


## Optional video service

See [media-video.md](media-video.md) for explicit installed-tool configuration, a real roundtrip
consumer, protocol tests and native fixture checks. `cargo test -p nodecraft-media --features video`
requires no FFmpeg installation; actual codec tests are explicit and never download tools.

Native `nodecraft-exec` tests use the pinned `allocation-counter` dev-dependency to measure
allocation-free metadata finalization. Its allocator applies to the entire exec test binary, including
unrelated tests, so coordinate any additional test allocator instrumentation. Production and wasm
allocators are unchanged.
