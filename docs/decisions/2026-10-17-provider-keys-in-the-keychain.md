## 2026-10-17 · Provider keys in the keychain (#80)

- **Keys live only in the OS keychain**, service `io.github.cyphernaught0x.nodecraft`, one
  account per provider (`openai`, `comfyui:<server>`), through `keyring` pinned exactly (it wraps
  the platform APIs; we add no `unsafe`). The browser build has no keychain yet: it uses an
  in-memory store until per-origin storage with a notice (M10).
- **`Secret` never prints its value** (`Debug` shows `Secret(…)`, `Display` at most `…abcd` for
  long keys), accepts only visible ASCII up to 1 KiB (it travels in HTTP headers; Windows caps
  keychain entries), and keychain errors never echo stored bytes. The value types live in
  `providers::secrets` so adapters take a `Secret`; the OS keychain stays in the engine.
- **Keys are tested for free** (listing models), never with a paid generation. `keys.test` (#237)
  sends one free GET per provider with a known free check, the key in an origin-pinned secret
  header, and answers verified, unverifiable or an `Authentication` error; it is human-only in MCP.
- **`keys.set` / `keys.remove` / `keys.status`** are commands with a `keys.store` host service:
  no document change, no undo step, results only ever show a masked tail. **`keys.set` is never
  offered to the in-canvas agent or MCP clients**: a key in tool-call parameters would end up in
  LLM transcripts and logs. Keychain calls can block on a prompt, so the app registers the
  service through `secrets::KeyWorker` (#205): the request is checked at once, run on a
  keychain thread, and answered through `KeyResults::drain` under the `pending` id it
  returned. In the app, `keys.*` commands therefore answer `{pending: id}`. The host holds the
  single `KeyResults` and routes each outcome by id to whoever asked (the keys panel, the in-app
  agent), and both queues are bounded. Each nonblocking drain returns at most 64 outcomes
  (`KeyResults::MAX_BATCH`, the queue capacity), retaining later results in order for subsequent
  calls even while the worker refills the queue. Headless hosts keep the synchronous service. Settings list the accounts that have
  keys (`[keys] accounts`, names only), so the keys panel never reads every key at startup.
