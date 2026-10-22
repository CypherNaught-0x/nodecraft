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
cargo xtask ci --quick               # the quick gate, for iterating (below)
cargo test -p nodecraft-ui-egui      # one crate
cargo nextest run --workspace        # parallel across test binaries
node --test scripts/claim/*.test.js  # the claim bot
```

### Quick and full

Run the **full** gate, `cargo xtask ci`, before you open a pull request or push to one. With
cargo-nextest installed its test step runs every test with `cargo nextest run --workspace`, then the
doctests with `cargo test --doc`; without nextest it falls back to `cargo test --workspace`, which
runs the test binaries one at a time and takes several times as long.

While iterating, `cargo xtask ci --quick` runs fmt, clippy and layers as the full gate does, and
then:

- tests only the packages changed since the merge base with `origin/main` (`--base REF` picks
  another), counting committed, staged, unstaged and untracked files, together with every package
  that depends on them (nextest's `rdeps()`; a stray untracked file outside every package therefore
  tests everything). A change outside every package that a build can read
  (`Cargo.toml`, `Cargo.lock`, `.config/`, `assets/`, …) tests everything; one only in `docs/`,
  `.github/`, `scripts/` or a top-level Markdown file tests nothing;
- leaves out the slow tests, the doctests and the wasm check.

Quick needs nextest. It compiles with the same `--workspace` and features as the full gate, so the
two share one build: Cargo resolves features per selection, and `cargo test -p <crate>` rebuilds the
crate's dependencies with a different feature set.

The slow tests are the generated block at the end of `.config/nextest.toml`: every test that took
at least five seconds in a full parallel run. The block also gives them a high nextest
`priority`, so every profile, CI's included, starts them first and the longest test does not finish
last. Regenerate it with `cargo xtask slow-tests [--threshold SECS]` after adding a heavy test or
when the list looks stale; it runs every test once and reads the timings from nextest's JUnit report.
Don't edit the block by hand.

### One integration test binary per crate

Each `tests/*.rs` file is normally its own binary, and each binary statically links the crate and
everything under it: engine's 67 files were 67 links of 40–170 MB each. Crates with more than one
integration test file therefore set `autotests = false` and build them as one binary,
`tests/it.rs`, which pulls each file in as a module with `#[path]`. The files stay where they are,
so a test in `tests/share.rs` is now `share::<test>` in the binary `nodecraft-engine::it`
rather than `<test>` in `nodecraft-engine::share`; run one file's tests with
`cargo nextest run -p nodecraft-engine -E 'binary(it) & test(/^share::/)'` or
`cargo test -p nodecraft-engine --test it share::`.

A test stays its own `[[test]]` when it must: one that docs or scripts run with `--test <name>`
(`panic_hunt`, `catalog_files`, the live provider tests that `docs/providers.md` names) or one that re-runs its own executable
by test name. Add a new file to `tests/it.rs`; `cargo xtask layers` and xtask's tests fail when a
`tests/*.rs` file is in neither, since Cargo would otherwise never build it.

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

Video import metadata (#572) has an opt-in real-tool contract in
`app::drop::tests::imported_video_lengths_reach_timeline_before_any_render`. With absolute
`NODECRAFT_VIDEO_TEST_FFMPEG` and `NODECRAFT_VIDEO_TEST_FFPROBE` paths set, run that test with
`-- --ignored`. It imports generated MP4/WebM/MOV files through real file-drop acquisition and
checks `duration_ms` and `timeline.list` clips' `media_us` before rendering (the issue's older
`video_us` field was generalized to `media_us`). A fragmented MP4 additionally
proves configured ffprobe fills an unknown header duration while missing tools leave it unknown.

GitHub CI runs `cargo nextest run --workspace --profile ci` and then `cargo test --doc --workspace`
on Linux, Windows and macOS. The workflow pins nextest and verifies each release archive against
a checked-in SHA-256 before extracting it. When updating nextest, update the version and all three
checksums together from the `.sha256` files published beside the archives on the
[official nextest release](https://github.com/nextest-rs/nextest/releases). `cargo xtask ci` uses
nextest when it is installed and `cargo test` otherwise, so the full local gate does not require it.

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

The report (`schema: "nodecraft.perf/3"`) gives, per metric, p50/p95/p99/max in microseconds,
the samples kept (the latest 4,096), recorded/invalid counts and an explicit availability reason.
Metrics:

- `frame_prepare`: shell CPU preparation, including workload collection;
- `frame_interval`: time between frame starts;
- `dispatch`: successful commands, including handlers and durable I/O;
- `graph_stage` and `graph_stage_refused`: successful and refused `Changeset::stage_certified` calls,
  respectively, including draft cloning, applying changes, validation, inverse construction and
  the inverse-certification probe;
- `undo`, `redo`, `load` and `save`: overlapping classifications of successful dispatch;
- `presentation`: presented-frame intervals, only from a host that names a
  `presentation_source`. The desktop's `--perf-run` names `vsync_frame_pump`: every frame asks
  for the next at once and presenting waits for vsync, so the time between two pumped frames is
  a presented interval. Its `dropped_frames` sums whole 60 Hz frames missed past 25 ms and
  `longest_drop` is the longest such run. Otherwise (normal use, the CLI, wasm) it has zero
  samples, null percentiles and `presentation_timestamps_not_available`.

Stage samples exclude handler work, subsequent actual-redo and replay/history/asset admission, durable append and final
publication. Successful staging followed by a later refusal still counts as executed staging;
preview/availability checks and nonmutating effects do not. Refusals are not mixed with successful
stage or dispatch latencies. These are overlapping boundaries, not additive phases.

The frame metrics count samples over 16,667 µs preparation and 1.5 times that between starts.
The app redraws on input: intervals over 250 ms are filtered as idle. These are interval-gap
heuristics, not observed dropped/presented frames. CPU preparation cannot prove 60 fps; the
pumped `presentation` metric can.

To time presented frames in a real window, run the desktop app in release mode:

```sh
cargo run -p nodecraft --release -- --perf-run perf-run.json
```

It opens generated 200- and 1,000-node graphs, settles each for 30 frames, then times 600 pumped
frames of steady panning (zoom 0.2) and of a zoom sweep (0.1 to about 1.0 and back), writes one
report per workload (`schema: "nodecraft.perf-run/1"`) and quits. Leave the window on screen and
uncovered: a hidden window may not be presented at all.

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
| `cargo xtask ci --quick [--base REF]` | fmt, clippy, layers and the changed packages' tests without the slow ones ([Quick and full](#quick-and-full)) |
| `cargo xtask slow-tests [--threshold SECS]` | time every test and regenerate the slow-test block of `.config/nextest.toml` |
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
and never open dialogs.

In the browser (#118), Open, Save and Save As use the File System Access pickers where the
browser has them (Chromium-based browsers), and save back to the picked file. Elsewhere, Open
uses a file field and every save is a download. The session's storage holds only the picked
file's bytes for one `file.open`, and the saved bytes until the browser writes them. Locations
are `<n>/<file name>` keys that are unique per pick. Leaving the page with unsaved work, or with
a save the browser failed to write, asks first. Projects over 64 MiB are refused before they are
read. `app/web_files/tests.rs` drives the flow through a fake browser port.

Unsaved work is also kept for recovery (#717). Each tab writes the session's recovery checkpoint
and journal to its own slot in the site's IndexedDB, and holds a Web Lock on that slot while it is
open. On load, the newest slot no open tab holds is offered back, as the desktop offers its
recovery file. A full quota is said once, and frames then wait for the next checkpoint
(`app/web_journal.rs`). `app/web_journal/tests.rs` drives reloads, other tabs, quota failures and
damaged slots over a fake shelf.

Recovery payloads share a 256 MiB retention budget across tabs (#726). Cleanup removes the oldest
unlocked copies first and reports how many were removed. Open tabs and a copy offered or kept
by this page are protected, even if their combined work exceeds the budget. Clean checkpoint-only
slots and orphan frames outside the eight-slot offer window are also cleaned up at load.
Pruning and new journal writes commit together; failed batches keep the previous copies. An
independent five-minute inventory refresh catches other tabs growing and closing, even while idle.
See [the budget decision](decisions/2026-10-22-browser-recovery-budget.md) for inventory bounds
and the behavior of concurrently growing tabs.

Runs work in the browser too (#118), through the same run host as the desktop. Its tasks run on
the page's queue, its sleeps are `setTimeout`s and its clock is `performance.now()`
(`app/runs/browser.rs`). Adapters are built at once from the keys in the page's storage, and are
handed over on the next frame as the desktop's keychain thread hands them over; the
`with_inline_maker` kittest covers that path natively. Providers are called directly from the
page, so only providers that accept cross-origin requests work. Anthropic is sent its
`anthropic-dangerous-direct-browser-access` header, and a request the browser blocks says the
provider may not accept requests from web pages. The browser's Providers panel saves ComfyUI and
LiteLLM server addresses per origin (#721), separately from keys. The servers must allow the
page's origin in their CORS settings; an HTTPS page may also block an HTTP server. Save creates
and persists the ComfyUI installation salt, and a failed save preserves the prior configuration.
`providers.settings_get` and `providers.settings_set` expose the same settings to commands.
`options.refresh` polls an owned future on the page and installs its result on a later frame;
an endpoint change drops a pending old-server reply and clears its cached lists/adapters. Native
kittests cover that async hand-over and per-origin persistence. Video tools and the run journal
still require the desktop.

Kittests inject picker futures and exercise actual native temporary-file persistence. They verify
UI routing and lifecycle/refusal behavior; they do not prove OS dialog operation on every platform.
