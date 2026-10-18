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

## Physical storage validation

Ordinary tests run only small same-path calibration:

```sh
cargo test -p nodecraft-engine --test physical_zip64147 physical_zip64_session_small_calibration -- --nocapture
cargo test -p nodecraft-format --lib physical_zip64_atomic_small_calibration -- --nocapture
```

The two ignored large tests each physically write/verify `u32::MAX + 2` asset bytes, without a
sparse file, reflink or giant buffer. They are manual resource-intensive proof, not ordinary CI.
Before running, review the exact frozen harness and simultaneous extent/reader/debt inventory,
obtain separate resource approval, refresh free space on the selected volume, and run these
commands sequentially in a dedicated existing parent directory. Each owned fixture is removed
before its test returns. The engine test must finish and clean up before the atomic test starts.

```sh
NODECRAFT_PHYSICAL147=1 NODECRAFT_PHYSICAL147_ROOT=/chosen/existing/parent CARGO_INCREMENTAL=0 cargo test -p nodecraft-engine --test physical_zip64147 physical_zip64_session_large -- --ignored --exact --nocapture
NODECRAFT_PHYSICAL147=1 NODECRAFT_PHYSICAL147_ROOT=/chosen/existing/parent CARGO_INCREMENTAL=0 cargo test -p nodecraft-format --lib storage::native::physical147::physical_zip64_atomic_large -- --ignored --exact --nocapture
```

Retain full logs, exact source/toolchain/binary provenance, successful write counters and per-phase
extent inventories. Source reads, logical reservations and allocated filesystem blocks are
separate measures. On 2026-10-18 both large commands passed sequentially on native macOS with
4,294,967,297-byte assets; the atomic fault followed 4,294,967,296 successfully forwarded file-write
bytes, above u32::MAX. Small calibration does not establish those results. The accepted physical
run does not replace final CI or later source-composition checks; future runs still need their
own resource review. See [storage.md](storage.md) for measured values and platform/RSS/device-write
limitations.
