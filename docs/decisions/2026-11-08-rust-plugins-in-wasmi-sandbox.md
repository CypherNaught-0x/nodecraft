## 2026-11-08 · Rust plugins: an in-process wasmi sandbox, not child processes (#1144)

[`2026-11-06-module-packages-refused.md`](2026-11-06-module-packages-refused.md) refuses
`rust_wasm` packages because no engine had been measured against the [plugin RFC](../plugins-rfc.md)
§11 gates. #1144 ran that trial for two directions: Rust compiled to WebAssembly and run in an
embedded sandbox, and native Rust plugins run as child processes and spoken to over RPC.

**Decision: Rust plugins are WebAssembly modules run in-process by [wasmi](https://github.com/wasmi-labs/wasmi)
(pinned `=2.0.0`), on desktop and in the browser. Out-of-process native plugins are rejected for
third-party code.** The refusal stays in force until the follow-up implementation lands with its
own conformance tests and the separate security review §11 asks for; that change replaces the
refusal record and flips `plugin_store::MODULE_REFUSED`.

### How it was measured

Throwaway spikes outside the repo (nothing here was built or vendored): a `#![no_std]` Rust guest
compiled to `wasm32-unknown-unknown` (4,062 bytes) exporting `noop`, an ASCII `normalize`, an
infinite loop, a memory bomb (`memory.grow` until refused), `unreachable` and unbounded
recursion; a wasmi host under `#![forbid(unsafe_code)]` giving each call a fresh `Store` with fuel
and a `StoreLimits` of 64 MiB memory and 1 instance; the same against wasmtime 50.0.1; and a
stdio parent/child using the [plugin-wire](../plugin-wire.md#frames-and-messages) framing
(4-byte big-endian length, at most 1 MiB) with a reader thread, a bounded queue, a deadline and
`kill`. Linux x86_64, 4 vCPUs, rustc 1.99.1, release builds, stripped with LTO. Node 22.23.4 (V8)
stood in for a browser engine. Windows and macOS were **not** run.

### In-process engines

| | wasmi 2.0.0 | wasmtime 50.0.1 (Cranelift) | browser's own WebAssembly |
|---|---|---|---|
| Licence | MIT/Apache-2.0 (deps: wasmparser 0.228 Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT, spin, libm, bitflags: MIT or MIT/Apache-2.0) | Apache-2.0 WITH LLVM-exception | n/a |
| `unsafe` inside the engine | 290 sites in `wasmi`, 41 in `wasmi_core`/`_ir`/`_collections` | 1,839 in `wasmtime` alone, plus Cranelift | n/a |
| Host code needs `unsafe`? | No: the spike host compiles under `forbid(unsafe_code)`. The only unsafe entry point, `Module::new_unchecked`, is then uncallable | Native: no. In wasm32 a module can only be loaded with `unsafe fn Module::deserialize`, and the custom platform needs `extern "C"` hooks (`wasmtime_tls_get`, `wasmtime_mmap_new`, …) | Needs `js-sys`/`wasm-bindgen` glue and a Worker to stop a guest |
| Builds for `wasm32-unknown-unknown` | Yes, with no imports (1.40 MB module including the guest) | No with std (`sys::mmap` missing); `runtime`+`pulley` checks but can't compile modules | Is the browser |
| Size added to a stripped binary | +1.27 MB (0.30 → 1.58 MB) | +10.0 MB (0.30 → 10.35 MB); build 4 min, 538 MB target dir | 0 |
| Compile guest | 0.11–0.14 ms (4 KB); 8–10 ms for a 1.4 MB module | 10 ms | browser-dependent |
| Fresh instance + no-op call | p50 63–81 µs, p95 94–117 µs | p50 6 µs, p95 10 µs | not measured |
| Infinite loop, 100 M fuel | "all fuel consumed" after 41–44 ms | out of fuel after 8.7 ms | no fuel: loops until the tab or Worker is killed |
| Memory bomb, 64 MiB cap | grows to the cap (976 pages after the initial ones), then `memory.grow` returns −1; the guest gets an error, the host nothing | same, 47 µs | only a module-declared maximum; the host can't cap it |
| Module declaring 512 MiB initial memory | refused at instantiation by the limiter | (not run) | |
| Trap / unbounded recursion | `unreachable` trap after 22 fuel; "call stack exhausted" in 0.15 ms | trap | |
| Garbage module | "malformed section id" error | (not run) | |
| 1 MiB normalize (copy in, run, copy out, fresh instance) | 17–18 ms, 80.7 M fuel | (not run) | |
| Time-slicing | `call_resumable` returns `OutOfFuel` and resumes: 1 M-fuel slices took p50 0.49 ms, max 0.63 ms; the host can cancel between slices by dropping the store | fuel or epoch yields through the async API, native only | |
| Running inside wasm32 | wasmi compiled to wasm32 and run under V8 kept both limits: 10 M fuel stopped the loop (p50 57 ms), a 16 MiB cap stopped the bomb, a 64 KiB normalize took 16 ms | can't | |

So wasmi runs about 13× slower under V8 than natively (10 M fuel: 57 ms against 4.4 ms) and about
5× slower than wasmtime's JIT natively. For pure CPU text and data actions under the RFC's 10 s
job ceiling that is acceptable; in these runs it meets the RFC §10 proposals it was measured against (warm no-op p95 ≤ 2 ms,
1 MiB round trip ≤ 20 ms in single runs, cancel ≤ 500 ms, ≤ 64 MiB per instance). Wasmtime's speed costs a
10 MB binary, 1,800+ internal `unsafe` sites, a JIT and no browser build, which would mean two
engines with different behaviour. The browser's own engine has no fuel, no host-set memory cap and
no way to stop a synchronous loop without JavaScript Worker glue, and it differs per browser.

**A trap to remember:** with `default-features = false` and no `auto-dispatch`, wasmi compiled for
wasm32 (which has no tail calls by default) recursed in its dispatcher and every non-trivial call
failed with `StackOverflow`. Keep the `auto-dispatch` feature; the follow-up tests it.

### Child processes over RPC

| Hostile child | Host saw | Kill and reap |
|---|---|---|
| never replies | `Timeout` at the 500 ms deadline | 0.4–0.6 ms |
| busy loop | `Timeout` | 0.3 ms |
| header claims 4 GiB, then streams | `TooLarge` at the header, nothing allocated | 0.2 ms |
| floods 1 MiB frames | one frame taken; the bounded queue back-pressures the rest | 0.2 ms |
| `abort` / panic / exits mid-frame | `Eof` with SIGABRT / status 101 / 0 | µs |
| starts a grandchild | the grandchild **survives** the child's kill | — |
| ambient access, after `env_clear()` | read `/etc/passwd` and listed the user's checkout, wrote outside its folder, opened an outbound TCP connection, bound a loopback port, spawned `/bin/true` | — |

Spawn to first reply p50 1.15 ms, p95 1.3–1.6 ms; warm round trip 1 KiB p50 35 µs, 1 MiB p50
2.3–2.5 ms. The host never panicked. The lifecycle is easy to get right; isolation is the problem:

- **A process is not a sandbox**, as the RFC says. The child holds every right the user has: it can
  read projects, assets and SSH keys, send them anywhere, and call paid APIs on its own. The
  broker can keep NodeCraft from *handing* it a key, but on Linux (Secret Service, once unlocked)
  and Windows (Credential Manager), another process of the same user can ask for NodeCraft's keys
  itself. "Secrets never reach a plugin" can't be promised.
- **Confining it needs per-OS sandboxes, each outside the no-`unsafe` rule as we'd use it.**
  Applying limits between fork and exec needs `CommandExt::pre_exec`, an `unsafe fn`. Linux could
  use a trusted launcher that applies Landlock and seccomp to itself and then execs the plugin.
  macOS has only the deprecated `sandbox_init` or App Sandbox entitlements on a signed bundle.
  Windows needs AppContainer and Job objects through FFI. That is three sandboxes to write, audit
  and test, and a memory bomb, a fork bomb or a surviving grandchild each need their own fix
  (rlimits, process groups, Job objects).
- **Distribution triples:** a package would ship one binary per OS and architecture, each to sign
  and review, against one portable `.wasm` with the same digest everywhere.
- **The browser has no processes**, so these packages could only be refused there.

Process isolation stays an option for *first-party* helpers NodeCraft ships and signs, not for
package code.

### What follows from it

- **Contract.** The guest talks the existing [plugin-wire](../plugin-wire.md) messages
  (`initialize` → `invoke` → `result`/`error`, canonical JSON frames ≤ 1 MiB) through its own
  linear memory, read and written by the host with wasmi's bounds-checked `Memory::read`/`write`.
  The guest gets no imports beyond what the host defines (no WASI, no clock, no network, no
  files) and never sees a key: paid work stays declarative through `provider_operations` and the
  broker, which shows the cost first and keeps budgets and idempotency keys.
- **Limits.** One fresh `Store` per job with fuel, a `StoreLimits` (memory ≤ 64 MiB, one instance,
  one memory, one table) and a capped call stack. Calibration from these numbers: about
  2.3 × 10⁹ fuel per second natively and 1.8 × 10⁸ under V8, so the follow-up sets one ceiling per
  job that ends within the RFC's 10 s in the browser, and runs in fuel slices so the UI never waits.
- **Placement.** wasmi goes in a new exec-layer crate that builds for wasm32, registered in
  `xtask/src/layers.rs`; the engine owns grants and lifecycle as for `text_actions`.
- **Exports need one `unsafe` attribute.** A guest must export its entry points with
  `#[unsafe(no_mangle)]`, which `forbid(unsafe_code)` refuses. A guest SDK macro hides it
  (verified: a guest crate under `forbid(unsafe_code)` builds through a macro from another crate),
  but the attribute then lives in that SDK's source. Where the SDK lives and how it squares with
  "no `unsafe`, anywhere" is for the owner to decide in the follow-up.
- **Not shown yet:** Windows and macOS conformance (wasmi has no OS-specific code; its build script
  only picks a dispatch method by architecture), a real browser rather than Node's V8, and an
  audit of wasmi 2.0: its two published audits (SRLabs 2023, 0.31.0; Runtime Verification 2024,
  0.36–0.38) predate the 2.0 rewrite. These, and the security review, are exit criteria for the
  follow-up, not assumptions.
