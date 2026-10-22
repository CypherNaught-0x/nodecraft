# Reproducing M1 performance measurements

`cargo xtask perf` is a release-only regression harness (#152). It does not run in ordinary
correctness CI, contact providers or measure GPU/display performance. The separate desktop
vsync frame-pump run (#526, #634) supplied the presented-frame proof used to close #65 on the
measured Apple M3 Pro/Metal 120 Hz machine. A headless result alone cannot close that criterion.

## Collect

From the checkout to measure, with a dedicated target directory:

```sh
CARGO_TARGET_DIR=target/perf-build CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 \
  cargo xtask perf collect --machine public-cpu-and-memory-class \
  --out target/perf/current --samples 120 --warmups 3
```

Use a public hardware class, not your hostname, account or project name. The report adds the
public CPU model, logical CPU count, OS/architecture, full rustc version/host and release profile.
Cargo/build/Rustflags configuration is fingerprinted, never printed. Version 2 of the envelope
adds actual Cargo version and selected-config provenance; old version 1 reports lack that evidence
and are rejected, not retroactively upgraded from today's files.

Cargo probes the invocation directory and its ancestors, then Cargo-home. The collector hashes
those ordered private locations and each selected `config` or `config.toml` file, honoring legacy
`config` precedence (the other file is ignored). Bounds: 32 ancestor directories plus Cargo-home,
4 KiB per input location, 64 KiB per selected regular file, 256 KiB total; 128 relevant environment
entries/64 KiB total.
All Cargo environment options are included conservatively except credential values and ephemeral
jobserver descriptors; Rust compiler/flags/toolchain and Rayon options are included. Windows
variable names are normalized for its case-insensitive lookup. Inputs are checked before and after
collection; a change refuses canonical publication. Relocated configuration locations can be
incomparable even with equal text, because relative paths can change meaning.

Supported configuration is deliberately finite: recursive `include`, dependency `paths`/`patch`/
`source` overrides, external resolver lockfiles, config-defined compiler/wrappers, compiler response
markers (`@`) in Rust compiler/doc flags, unstable Cargo options and unknown top-level keys are refused. Use an explicit `RUSTC`
when choosing a compiler, whose version is recorded. This fingerprints selected Cargo inputs;
it does not claim transitive build-script/external-tool/filesystem hermeticity. Build tools,
transparent instrumentation wrappers and their other inputs remain trusted and must stay fixed.
See [Cargo's configuration precedence](https://doc.rust-lang.org/cargo/reference/config.html).

The output directory must be new; within the checkout it must be Git-ignored. Nothing uploads
from the local command. A previous baseline is never overwritten automatically. Missing or
unreadable source inputs refuse collection; stage an intentional deletion or restore a readable
input rather than omitting it from the fingerprint. Config-defined compiler and wrapper overrides
have a category-specific refusal; their private values and paths are not included in the error.

The tool resolves the invoking workspace, fingerprints its tracked and unignored source bytes
and executable modes, and records full revision/dirty state. It cleans only workspace **release**
packages before building, retaining third-party dependencies and debug outputs; this prevents
cross-checkout workspace cache reuse being mistaken for current-source measurements. It obtains
three exact release test executables from Cargo JSON, records their content digests, retains copies
under `local-binaries/`, and invokes only these ignored tests with `--exact --test-threads=1`:

- graph `structural_chain_apply_undo_redo_benchmark` (the migrated existing benchmark);
- engine `perf152_hot_paths`;
- UI `app::perf::regression152::canvas_regression`.

Each build has a 30-minute watchdog; each workload has 15 minutes. Logs are monitored against
64 MiB, not a claim of an exact physical disk-write ceiling. This is trusted-workspace build
orchestration, not a sandbox for hostile build scripts or a process-tree containment guarantee.
Raw suite files and local execution/compiler logs may contain build paths; publish only `report.json`,
`summary.md` and comparison reports. The canonical report has no document contents, prompts,
asset bodies, secrets, usernames, hostnames or private paths.

## Required matrix and timing boundaries

All elapsed samples use monotonic wall-clock time, including categories named CPU for their
CPU-side work; they are not process CPU utilization or hardware cycle measurements.

All four deterministic topologies (chain, fan-in, fan-out and disconnected) run at **200 and
1,000 nodes**, across graph apply/undo/redo, registered command dispatch and project save/load.
The many-input relay is a test-only registered kind, not a product API. Ordinary tests verify its
ports and real command/storage round trips. The UI's landed #464 200-node row fixture reuses the
same command builder while retaining its original 401-step Undo and reopen assertions.

- Graph spans are `Changeset::apply` on already constructed changesets, including its validation
  and inverse work; construction and equality/round-trip assertions are outside the span.
- Dispatch spans are real registered `node.move` on an ephemeral Session. Expected state,
  untimed Undo/reset and assertions are outside the span.
- Memory byte-host and native host save/load are separate wall-time categories. Spans include
  the synchronous engine file command, publication and checkpoint work. Native tests use small
  real `.ncraft` files, warm same-owner reopen, and the host's actual atomic/fsync policy; they do
  not claim cold-device I/O timing. Fixtures contain no media assets, and that count is explicit.
  Native small-format manifest version is observed from real encoded bytes. Temporary files,
  construction and resulting document/assets/history checks are outside the span. An async UI
  submission frame is **not** a durable-save completion measurement.
- Canvas uses #65's actual graph, pan/zoom and frame loop: 1,600×1,000 points, 1/2 pixels per point,
  120 pan or 160 zoom frames after ten instrumented warm frames. The 200-node minimum-zoom pan
  asserts all nodes are visible; the 1,000-node full-zoom pan asserts mostly offscreen. Zoom
  sweeps retain actual counts rather than claiming all nodes remain visible. The timed span is
  egui/AccessKit plus shape cloning/CPU tessellation, including enabled capture callbacks and
  workload scans. Untimed per-frame report observation/export is excluded; it can still affect
  subsequent cache state. This is not a capture-overhead estimate; the separate #371 paired
  full-path workload remains available for that purpose.

There are **72 required records**. Missing/duplicate combinations, lost visibility observations,
unsupported required clocks or invalid data are incomplete evidence, never an attractive speedup.
Each canvas record consumes `nodecraft.perf/3` from a headless kittest window, which has no
presentation source, so Presentation stays explicitly unsupported there (the 60 fps criterion
above is the pumped desktop run). GPU timing, presentation timing and wasm runtime measurements are
listed as unavailable.
Actual desktop `perf.report` exports remain separately labeled host CPU diagnostics and cannot
be compared directly with headless tessellation spans or substituted for presentation timestamps.

## The 60 fps criterion

M1's "a 200-node graph at 60 fps" (plan §7, #65) is tested with presented frames, not CPU
preparation (#526). Run the desktop app's pumped run in release mode, with its window on screen:

```sh
cargo run -p nodecraft --release -- --perf-run perf-run.json
```

Each workload times 600 presented frames (10 s at 60 Hz) after 30 settle frames; see
`docs/development.md` for what `presentation`, `dropped_frames` and `longest_drop` include. The
criterion holds on a machine when, for the 200-node graph:

1. **P1 (pan):** steady panning at zoom 0.2 drops at most 6 frames (`dropped_frames ≤ 6`, 1% of
   600) and has a p99 presented interval ≤ 25 ms.
2. **P2 (zoom):** the zoom sweep from 0.1 to about 1.0 and back meets the same two bounds.
3. **P3 (hitches):** in both, no hitch drops more than 2 consecutive 60 Hz frames
   (`longest_drop ≤ 2`).

The bounds are on drops, not on a 16.7 ms percentile: a smooth 60 Hz display shows frame-start
intervals scattered a fraction of a millisecond either side of 16.7 ms, so about half of them
are "over" with nothing dropped. An interval counts as a drop past 25 ms, 1.5 frames.

The 1,000-node graph is reported alongside it but isn't part of the criterion. The run counts
drops against 60 Hz on any display. A faster display shows shorter intervals, not a higher bar.
A report without a `presentation_source` (a host that couldn't promise vsync) proves nothing
either way. The headless `cargo xtask perf` canvas records below remain CPU regression evidence
and can't stand in for this run.

#634's two release-mode desktop runs on an Apple M3 Pro/Metal **120 Hz** display observed p99
presented intervals of 8.65–8.79 ms (200-node pan) and 8.70–8.95 ms (200-node zoom), with
0 and 1–2 dropped frames respectively and longest drops of 0 and 1–2. The 1,000-node pan/zoom
runs also met P1–P3. CPU *frame preparation* p95 in those runs was 0.58–0.84 ms at 200 nodes
and 1.00–2.19 ms at 1,000 nodes: those are **not** CPU preparation-plus-tessellation numbers
from the headless benchmark. This is measured evidence on that machine, not proof on 60 Hz,
Windows or Linux hardware. The #634 PR states the measured run preceded its review corrections
(which did not change the hot path); no new current-head measurements are asserted here.

The older UI kittest figures (1.90 ms at 200 nodes and 5.4/6.39 ms at 1,000) were CPU egui
preparation **plus tessellation**, measured at `481700ba` on 2026-10-17, not present intervals.
At `aa1efc86` a quiet-machine repeat found 17.09 ms at 200 nodes and 283.55 ms at 1,000
for an after-edit frame, so treating the older figures as current would have concealed a real
regression (#549). #551 fixed quadratic live-action planning and reported 2.28/7.84 ms on
Apple M3 Pro at load ~18, using `node.move` (which now takes the settled-layout fast path).
That busy-machine result is neither a quiet-machine comparison with `481700ba` nor a benchmark
of full non-layout parameter-edit planning. Do not extrapolate it into a current general edit
or displayed-frame claim; rerun a named workload and record its commit and machine.

## Compare and establish a baseline

```sh
cargo xtask perf compare /path/to/intended-base/report.json target/perf/current/report.json \
  --out target/perf/comparison
```

Collect the intended base with this same harness/workload version, not an arbitrary old commit
that lacks it. Keep its complete report immutable. Baseline revision and candidate revision are
expected to differ; environment, CPU/model/class, toolchain/target/profile/flags, fixture/dependency
fingerprint, capture/renderer/capability metadata and observed workload counts must match.
An optimization that changes required geometry/counts or workload definitions needs a deliberate
new workload version/baseline, not a favorable old comparison.

Raw ordered nanosecond samples are retained; sorted copies supply nearest-rank p50/p95/p99,
range and median absolute deviation. Defaults are three warmups/120 samples for headless cases.
`--warmups` is 1..16 and `--samples` 3..256; smaller sample counts are useful for smoke checks,
but comparison refuses fewer than 100 observations because p99 is under-sampled. Even at 120,
p99 is descriptive near-max, **not** a confidence interval or reliable population tail estimate.
Zero elapsed values are permitted; zero sample counts are not. Relative change against a zero
baseline is explicitly unavailable, with absolute differences still reported.

The provisional **informational** policy `baseline-quantile-plus-3iqr-v1` compares p50 with
p50, p95 with p95 and p99 with p99. Each workload's budget is its measured baseline quantile
plus three times its baseline IQR (nearest-rank p75 minus p25). The factor three is a conservative
engineering allowance for the observed central spread, **not** a confidence bound or stable SLA.
Every comparison exports the actual per-workload numeric budgets, baseline distributions and
policy version in `comparison.json`, and a readable table plus sample/range/IQR/MAD summaries.
A lone scheduler interruption cannot automatically inflate all budgets; max/range remain visible.
A large IQR still gives poor sensitivity and is reported rather than called stable.
For canvas, the spread also includes changing script/visibility phases; it is not a pure machine-jitter
estimate. Whole-script budgets can therefore be broad even on a quiet host.

Initial shared-machine measurements exposed range/median ratios up to 135.46 in dispatch,
29.54 in memory storage and 8.75 in native storage. Using the whole range as an allowance would
hide meaningful regressions; it was rejected. That historical version 1 report remains diagnostic
only because its external Cargo configuration was not captured. A corrected version 2 collection
supplies the initial comparable per-workload
baseline; no approved blocking budgets or measured cross-revision performance win is implied.
Repeat controlled runs when establishing a stable-machine policy, document why its budgets are
useful, and version policy changes deliberately. Never calibrate against a known degraded
candidate or auto-rewrite a baseline after a regression. Shared-runner results stay informational.

Exit codes: **0** collected or all measured quantiles within observed envelopes; **2** measured
regression signal; **3** malformed, incomplete, incomparable or failed execution. A collection
without a requested baseline says **not compared**, not a regression pass. Comparator tests prove
an unchanged pair, median-preserving tail slowdown, zero baseline, missing/duplicate records,
under-sampled tails, unavailable metrics, incompatible environments and hostile JSON admission.
A collection that cannot read a CPU model records `unavailable` and is never compared at all, even
against an identical baseline; that refusal is separate from environment equality.
The readable comparison identifies both source revisions/fingerprints, definition fingerprints and
dirty flags, plus the shared public machine/CPU class. A dirty-source warning requires using the
recorded fingerprint rather than treating HEAD as the complete measured tree. Only these existing
public envelope fields are rendered; free-form text is escaped and its length bound is measured in
bytes, not characters. Private build configuration is never included.
Input is capped at 8 MiB with depth/string/token preflight before owned JSON parsing; records and
ordered samples are separately bounded. These are tooling limits, not process-RSS guarantees.

To update a baseline: choose a clean intended source revision and public hardware class; collect
into a new directory; inspect correctness/counts, sample distributions and provenance; retain
that report with its source identity and a reason for replacement. Review the policy/data change
with the PR. Toolchain, dependency, workload or hardware changes are often incomparable and call
for a fresh baseline rather than relaxed validation. The fixture/dependency fingerprint deliberately
includes the workspace manifest and Cargo.lock: dependency or workspace-version changes may refuse
comparison even when the timed source is unchanged. This conservative boundary is intentional; there
is no dependency-change bypass. Bulky run artifacts stay ignored.

## CI and investigation

`.github/workflows/performance.yml` offers manual/weekly collection and uploads machine-readable
and readable artifacts. A manual run may select a previous successful performance workflow run
ID as its explicit baseline. Malformed/incomplete/incomparable evidence fails the step; a measured
slowdown is visibly reported but informational on shared hardware. Without a selected baseline,
the workflow explicitly publishes only collection, no comparison verdict. Actions and Rust 1.99.1 are
pinned; that toolchain was observed in PR #478's successful hosted Linux correctness job. Updating it
requires a new comparable performance baseline. Shared GitHub runner CPU models can also change;
exact environment comparison then refuses rather than equating different hardware.

The first hosted performance workflow run (GitHub Actions run 39341922791, source
`8898cd2254714c7efd1fd0f53554db49810f15df`) completed successfully on 2026-10-19.
That one collection is not evidence of recurring baseline stability or a cross-revision win.
Retain its actual report before changing operational limits; ordinary correctness CI remains
separate from this release-mode performance workflow.

Investigate a signal by checking the compared revision/fixture/environment identities, actual
counts and supported metrics first, then raw sample order/range/MAD and each matching quantile.
Repeat on a quiet stable machine when appropriate; never mask a failed fixture, empty report or
incompatible environment with retries, timing assertions or automatic baseline updates.
