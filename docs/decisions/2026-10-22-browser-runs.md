## 2026-10-22 · Runs in the browser use the desktop's run host and call providers directly (#118)

- **One run host.** The browser runs cards through the same `RunHost`, quotes, budgets and
  idempotency keys as the desktop, so cost-first and no-double-charge hold in both. Only the
  runtime differs: `wasm-bindgen-futures` for tasks, `setTimeout` for sleeps and
  `performance.now()` for the clock. `std::time` panics in the browser, so nothing on this path
  uses it.
- **Adapters are built inline.** The desktop builds adapters on a thread because the keychain
  may prompt. The browser's key store answers at once, so it builds them inline, but they are
  still handed over on the next tick so both share one flow. The `Maker` enum keeps both, and
  native tests drive the inline variant.
- **Direct calls, no proxy.** The person's key stays in their browser and goes only to the
  provider. A proxy would see every key and prompt. The cost is that providers which refuse
  cross-origin requests don't work from the page. The network error says so rather than
  pretending the provider is down. Anthropic needs `anthropic-dangerous-direct-browser-access`,
  which is sent only by the wasm build. The danger it names is a key exposed to other people's
  pages, and here it is the person's own key on their own page.
- **Deferred:** ComfyUI and LiteLLM endpoints and option lists need browser settings and an async
  fetch (#721). Video tools and the run journal need a filesystem and stay desktop only.
