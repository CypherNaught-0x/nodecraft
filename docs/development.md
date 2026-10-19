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

The report (`schema: "nodecraft.perf/1"`) gives, per metric, p50/p95/p99/max in microseconds,
the samples kept (the latest 4,096), how many were recorded and how many were refused as invalid.
Metrics:
- `frame_prepare`: CPU time in the shell per frame;
- `frame_interval`: the time between frame starts;
- `dispatch`: any command;
- `undo`, `redo`, `load` and `save`.

The two frame metrics also count frames over budget: 16,667 µs to prepare, and 1.5 × that
between frames, which leaves room for scheduling jitter. The app redraws only on input, so an
interval over 250 ms is counted as `idle` rather than kept as a slow frame. Only commands that
ran are timed; refusals aren't. `workload` gives the last frame's visible and total nodes and
edges. `environment` adds the CPU count and, in the desktop app, the GPU adapter and backend. `frame_prepare` excludes GPU work and
presentation, so it can't prove 60 fps on its own. A metric without a clock (the browser build
for now) is `supported: false` with `null` values, never zero. Recording costs one bounded push
per sample (about 90 ns in a debug build). `cargo test -p nodecraft-engine --lib recording_overhead -- --ignored --nocapture`
prints the cost with capture on and off.

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

### Checking stale canvas results

Double-click empty canvas space, search for **List stale results**, and choose it to run the
existing `canvas.stale` command for that canvas. The bottom-left notice shows the last explicit
check, its stale-result count and the number of checked results whose inputs could not be hashed.
A diagnostic distinguishes the checked result from its failing input node. Only results with
recorded hashes are hash-checked; downstream staleness can propagate, and unrecorded results are
not verified fresh. Changing the document, canvas or session clears the display. View changes
and Save can retain the historical check; rerun it after changing external configuration.
