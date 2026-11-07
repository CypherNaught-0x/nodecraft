## 2026-11-06 · LLM calls go through a maintained client crate: rig-core (#489)

Status: accepted by the owner on 2026-11-07, choosing the RFC's option B ("depend on a client
crate"), with the crate left to this record. NodeCraft is not a paid product, so the owner relaxed
the RFC's guarantees in favour of provider breadth and features over maintaining dialects or a
gateway of our own.

### The crate

**`rig-core`**, pinned exactly (`=0.46.0` at the time of writing), with `default-features = false`
and never its `reqwest` feature. Chosen over `liter-llm` because:

- it has a transport seam: its HTTP client is a trait the host implements, so requests go
  through NodeCraft's own `nodecraft_providers::Http` (keys from the keychain, the
  `NodeCraft/<version>` User-Agent, no redirects, response caps, the browser's `fetch`), and
  without `reqwest` it pulls no tokio, hyper or TLS stack;
- it builds for wasm32 that way, so the browser keeps LLM calls; `liter-llm` builds its own
  client and its budget layer doesn't build for wasm;
- its source has no `unsafe` and no panics in request paths (the RFC's measurements), which
  AGENTS.md's crash rules need from a dependency on every chat path.

Its MSRV (1.97.1) is above the workspace's declared 1.95: adopting it raises `rust-version`, in
the same change that adds it.

### What stays NodeCraft's, relaxed where the owner allowed

- **Money.** Quotes, budget admission and the usage log stay in the app, from the catalog's
  prices, never `rig-core`'s `f64` catalog. Reported usage is turned into `Money` at the boundary
  with the same bounded conversion `llm::usd` uses today.
- **Delivery, relaxed.** The RFC wanted a "never dispatched" proof the crate can't carry. Instead
  every error from a request that may have been sent is treated as possibly sent: a chat request
  is not retried automatically after it, and its charge is held as unresolved at its quote. That
  is conservative without needing the crate's cooperation.
- **Identity.** The User-Agent is set by our transport, which overrides the crate's header; no
  originator or session header is sent.
- **Boundary.** `rig-core` types stay inside `nodecraft-providers` (layer 3): nothing crosses
  into `engine`, `agent` or `ui-egui`, and nothing from it is persisted in `.ncraft` files or the
  usage log. `cargo xtask layers` gets a check for that.
- **Surface.** Every provider module still compiles in (the crate has no feature gates); only
  those NodeCraft configures are ever called. This is the one RFC concern accepted as is.

### Migration

The hand-written `Dialect::OpenAi` and `Dialect::Anthropic` paths are replaced provider by
provider behind the existing `Provider` / `LlmClient` interface, each with its fixture contract
tests kept passing against the mock transport, so cards, the agent and the CLI don't change.
Then the long tail the RFC listed (native Bedrock Converse, Vertex chat, the rest of the crate's
providers) can be added as catalog entries rather than adapters. The work is tracked in its own
issue, opened with this record.

### Supersedes

The RFC's open questions are answered by the owner's choice. The earlier proposal in this PR
(keep our own dialects) is withdrawn.
