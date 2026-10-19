## 2026-10-19 · Bounded release performance envelopes (#152)

The M1 regression harness reuses graph and canvas workloads plus the landed command-built save
fixture. It lives in Rust xtask/test tooling, not a product command or another recorder. Exact
release executable filters avoid running unrelated ignored/live/GPU tests. The versioned
`nodecraft.bench/2` envelope retains ordered samples, `nodecraft.perf/2` availability, required
matrix/counts, source and executed-binary identities and public environment metadata.

Comparisons require identical workload/dependency and environment definitions while allowing
source revisions to differ. They compare like-for-like p50/p95/p99 and reject under-sampled,
missing or unavailable required evidence. Provisional policy `baseline-quantile-plus-3iqr-v1`
sets each workload's matched-quantile budget to its baseline quantile plus three times its measured
interquartile range. The conservative engineering multiplier is not a confidence bound or stable
SLA; all numeric budgets, range/IQR/MAD and sample counts are explicit. Actual shared-machine
measurements rejected whole-range allowances because isolated extremes inflated them up to135x
the median. Large IQR can still hide slowdowns, so initial comparisons remain informational.
Stable-machine budget replacement is deliberate and never automatic after a regression.

Headless egui preparation/tessellation, engine CPU, memory host wall-time and native host wall-time
remain separate. Unsupported presentation/GPU/wasm runtime is explicit. #65 retains its desktop
60-fps criterion. See [reproduction and baseline policy](../performance.md).


Comparison also requires the actual Cargo version and a bounded fingerprint of selected workspace,
ancestor and Cargo-home configuration (legacy-name precedence), supported environment options and
private input locations. Includes/dependency redirects and other unsupported indirections are
refused rather than called transitively covered. Inputs must remain fixed through collection.
Version1 measurements lacking this provenance remain diagnostic; present-day observations do not
retroactively prove their historical build inputs. See the supported constraints in the reproduction
documentation. This is not a hermetic external-tool/build-script guarantee.
