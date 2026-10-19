# Reproducing M1 performance measurements

`cargo xtask perf` is a release-only regression harness (#152). It does not run in ordinary
correctness CI, contact providers or measure GPU/display performance. #65 owns canvas optimization
and its desktop 60-fps criterion; a headless result cannot close that criterion.

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
from the local command. A previous baseline is never overwritten automatically. Missing/unreadable
source inputs refuse collection; stage an intentional deletion or restore a readable input rather than omitting it from
the fingerprint. Config-defined compiler/wrapper overrides have a category-specific refusal; their
private values and paths are not included in the error.

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
Each canvas record consumes `nodecraft.perf/2`; Presentation remains explicitly unsupported.
GPU timing, presentation timing and wasm runtime measurements are listed as unavailable.
Actual desktop `perf.report` exports remain separately labeled host CPU diagnostics and cannot
be compared directly with headless tessellation spans or substituted for presentation timestamps.

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
only because its external Cargo configuration was not captured. A corrected version 2 collection supplies the initial comparable per-workload
baseline; no approved blocking budgets or measured cross-revision performance win is implied.
Repeat controlled runs when establishing a stable-machine policy, document why its budgets are
useful, and version policy changes deliberately. Never calibrate against a known degraded
candidate or auto-rewrite a baseline after a regression. Shared-runner results stay informational.

Exit codes: **0** collected or all measured quantiles within observed envelopes; **2** measured
regression signal; **3** malformed, incomplete, incomparable or failed execution. A collection
without a requested baseline says **not compared**, not a regression pass. Comparator tests prove
an unchanged pair, median-preserving tail slowdown, zero baseline, missing/duplicate records,
under-sampled tails, unavailable metrics, incompatible environments and hostile JSON admission.
The readable comparison identifies both source revisions/fingerprints, definition fingerprints and
dirty flags, plus the shared public machine/CPU class. A dirty-source warning requires using the
recorded fingerprint rather than treating HEAD as the complete measured tree. Only these existing
public envelope fields are rendered; private build configuration is never included.
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

The first hosted performance workflow execution is still pending. Ordinary hosted correctness CI
and local release collection do not establish its Ubuntu runtime, build-watchdog margin or recurring
baseline stability. Run it deliberately after the workflow exists on the default branch, retain its
actual result, and adjust operational limits only from that evidence. No hosted performance pass is
claimed here.

Investigate a signal by checking the compared revision/fixture/environment identities, actual
counts and supported metrics first, then raw sample order/range/MAD and each matching quantile.
Repeat on a quiet stable machine when appropriate; never mask a failed fixture, empty report or
incompatible environment with retries, timing assertions or automatic baseline updates.
