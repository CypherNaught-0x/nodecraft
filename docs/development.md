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

## xtask

| Command | Does |
|---|---|
| `cargo xtask layers` | print the crate → layer table and fail on layering violations |
| `cargo xtask wasm` | `cargo check --target wasm32-unknown-unknown` for every layered crate |
| `cargo xtask catalog-check` | validate `assets/catalog/*.toml` and that each file is built in (`docs/catalog.md`); also part of `cargo test` |
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
