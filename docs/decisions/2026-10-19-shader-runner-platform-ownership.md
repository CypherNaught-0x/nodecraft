## 2026-10-19 · Shader runner ownership follows the platform (#453)

Native custom shaders retain one process-wide runner behind the existing mutex. The whole
shader execution and staging section remains serial, and a lost runner is replaced on the
next invocation. Pipeline limits, error handling and GPU-only behavior are unchanged.

On `wasm32`, browser GPU handles are not required to be `Send` or `Sync`. Keep one runner per
thread in thread-local storage, with fallible access and mutable borrowing. Reentrant access
or unavailable thread-local storage returns an error before using the slot, rather than
panicking. A future host using multiple workers has separate caches; this is not a global
cross-worker memory bound. No unsafe implementation or fragile Send/Sync feature is added.

This fixes independent library compilation, not browser shader execution. The existing
synchronous runner still needs adapter/device requests to complete immediately and otherwise
reports GPU unavailability; it does not implement an asynchronous browser GPU host. Native
tests do not establish browser rendering support.

The wasm gate first checks `nodecraft-automation` alone (and thus engine) without the UI,
then retains the combined library and opt-in mock checks. The separate invocation prevents
UI dependency features from masking an invalid global browser-GPU owner.
