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

Native file-drop tests live under `app::drop` in `ui-egui`. They use isolated instances of the
same fixed-worker machinery with bounded backend gates, fake monotonic clocks and real temporary
files. On Unix one test drops a real FIFO with no writer, which the non-blocking open refuses
at once; none waits on a hanging one. Run `cargo test -p nodecraft-ui-egui --lib app::drop`.
The ordinary UI tests include persistent Session Open/Save, cancellation, stale delivery, pointer
ownership, captured placement and one-step undo. Acquisition tests cap reported read work and
owned buffer capacity; those counters are not total process RSS or physical device I/O. Windows
and other platforms still need their native CI; local macOS results do not prove them.

GitHub CI runs `cargo nextest run --workspace --profile ci` and then `cargo test --doc --workspace`
on Linux, Windows and macOS. The workflow pins nextest and verifies each release archive against
a checked-in SHA-256 before extracting it. When updating nextest, update the version and all three
checksums together from the `.sha256` files published beside the archives on the
[official nextest release](https://github.com/nextest-rs/nextest/releases). `cargo xtask ci` continues to use `cargo test`
so local checks do not require nextest.

A pull request that changes nothing but documentation and repository tooling skips those three legs:
a `rust scope` job classifies the change set and the three `rust (<os>)` legs run on its answer.
`scripts/ci/rust-scope.sh` holds the whitelist and is deliberately the short list — anything unknown
builds — because a change to a file the build reads (a manifest, `clippy.toml`, `.gitattributes`,
anything in `assets/`) can break tests while touching no code. Only a pull request based on `main` may
skip, the classifier CI runs is the copy committed on the base revision, that copy runs before CI runs
anything the pull request wrote and in a scrubbed environment, and a change anywhere in `scripts/ci/`,
`.github/workflows/` or `.github/actions/` forces a full build — together, the four things that stop a
pull request rewriting the whitelist to excuse itself. The legs are three named jobs rather than one
matrix, because a conditional that stops a matrix from expanding publishes an unexpanded job name and
the required checks then never report; they share `.github/actions/rust-leg/action.yml`. The
classifier's own tests, `scripts/ci/rust-scope.test.sh`, run in the scope job *after* the decision is
recorded, and they assert that ordering along with the rest of the wiring. Skipped change sets get a
`docs gate` job instead: `git diff --check` over the same merge base, plus `cargo xtask decisions` —
which the Linux leg also runs, since `cargo xtask layers` does not include it on its own. Pushes to
`main` and manual runs always build everything, and `claim bot tests` is unconditional and fails if its
test files disappear. The reasons, including why this is a job and not `paths-ignore`, are in
[the decision record](decisions/2026-10-18-rust-ci-skips-prose-only-changes.md).

**Until `rust scope` and `docs gate` are added to the branch ruleset's required status checks, a
docs-only pull request can merge with nothing required having checked its contents.** [#425](https://github.com/CypherNaught-0x/NodeCraft/issues/425)
tracks that edit, and it has to happen **after** this workflow reaches `main`: ruleset strict mode
means a required context that a branch's workflow never publishes leaves that pull request Pending
forever, so adding the contexts first would freeze every open pull request. Land the workflow, let the
open pull requests pick it up, then add the two contexts. The four there now keep their exact names.

## Benchmarks

For the bounded release collect/compare matrix, baseline policy, CI artifacts and precise timing
boundaries, see [performance regression measurements](performance.md). `cargo xtask perf help`
prints the tooling entry points. The older manual workloads below remain available.

- `cargo test -p nodecraft-ui-egui --release perf -- --ignored --nocapture`: canvas frame time over
  200- and 1,000-node graphs (see `docs/ui-design.md`, "Level of detail and performance"). The
  ignored `app::tests::perf281_visual_current` render capture is opt-in: with
  `NODECRAFT_PERF281_RENDER_ROOT` unset it prints why it skipped and the run still passes, and with
  it set to a directory it cannot capture into the run fails loudly.

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
- `graph_stage` and `graph_stage_refused`: successful and refused `Changeset::stage_certified` calls,
  respectively, including draft cloning, applying changes, validation, inverse construction and
  the inverse-certification probe;
- `undo`, `redo`, `load` and `save`: overlapping classifications of successful dispatch;
- `presentation`: unsupported until a host supplies real presentation timestamps. It always has
  zero samples, null percentiles and `presentation_timestamps_not_available`, even with a CPU clock.

Stage samples exclude handler work, subsequent actual-redo and replay/history/asset admission, durable append and final
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
| `cargo xtask wasm` | `cargo check --target wasm32-unknown-unknown` for every layered crate and the `nodecraft-web` app |
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

### Recovery safety checks

Desktop recovery retains every unselected copy, including divergent snapshots with the same
project id and path. A recovered copy is retired only after an actual save of its still-current
document lineage. Opening another file does not count as saving it. If the recorded last save
cannot be opened, the selected copy remains and the notice names it. At 99 occupied canonical
slots startup refuses before replacing the live journal; do not remove unknown copies automatically.

The recovery kittests cover real native journals, streamed assets above the classic byte cap,
programmatic Open/Recover between frames, failed publication, and modal navigation ownership.
The ignored `render_recovery_safety450_evidence` test records public-safe warning and failed-open
frames when `NODECRAFT_REVIEW_EVIDENCE` names an output folder and
`NODECRAFT_450_EXPECT_RETAINED=1`. Captures are local evidence and must not be committed.

## Desktop project files

Use **File** for Open, Save, Save As and recent projects, or Cmd/Ctrl+O, Cmd/Ctrl+S and
Cmd/Ctrl+Shift+S. The same three actions are in the quick-add palette. An untitled or read-only
project uses Save As. Cancelling a picker changes nothing. Opening over unsaved work asks whether
to Save, Discard or Cancel; a failed or cancelled Save keeps the current project open. Finish or
discard uncommitted card-field drafts before file actions.

Recent projects are kept in local settings (16 entries); missing files report an error and can be
removed individually or cleared. File picks and recents still pass native storage admission; there
is no small-file buffering path in the UI. Programmatic file commands always require locations
and never open dialogs. The browser preview does not yet provide project storage or file pickers.

Kittests inject picker futures and exercise actual native temporary-file persistence. They verify
UI routing and lifecycle/refusal behavior; they do not prove OS dialog operation on every platform.
