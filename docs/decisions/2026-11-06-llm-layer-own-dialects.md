## 2026-11-06 · LLM calls keep our own dialects; gateways stay endpoints; catalogs may become data (#489)

Status: proposed. #489 asks for a decision record, not code, and that decision is the owner's to
take. This record proposes one, answers the RFC's open questions from the code as it is today,
and lists what would change it. Nothing here changes behaviour.

### Proposal

**Options A + D from the RFC, with the catalog half of D as the only new work:**

- **A. Keep hand-written dialects**, `Dialect::OpenAi` and `Dialect::Anthropic` in
  `crates/providers/src/llm/`, restructuring only when a third wire shape is actually needed.
- **D. A gateway stays one endpoint** (`Endpoint::litellm`, custom base URLs #960), never the
  architecture and never operated by the app.
- **Catalogs as data:** model lists and prices may be generated from a pinned, provenance-stamped
  upstream snapshot into `assets/catalog`, checked by `cargo xtask catalog-check`, without taking
  anyone's client code.
- **B (depend on a client crate) is declined** while any of the four guarantee failures the RFC
  verified in `rig-core` 0.45.0 stands. **C** (clean-room the shape) is what A becomes the day a
  third dialect is needed; it isn't started speculatively. **E** is what A + D already amount to
  for the client half.

### Why, from the code as it is now

- **The long tail shipped without a layer.** The RFC's strongest argument for a layer was the
  queue of LLM adapters #44–#48 (native Gemini, Azure OpenAI, Bedrock, Vertex, Hugging Face) and
  the agent (#114) needing a second backend. All six are **closed**. `llm/` now holds `openai.rs`,
  `anthropic.rs`, `azure.rs` and `bedrock.rs` over the same two wire dialects plus SSE, 2,661
  lines with their tests. The adapters a layer would have bought were built in a few hundred
  lines each, inside the money-safety contract, with no upgrade tax.
- **The line that doesn't move is still the deciding fact.** Quote before pay, budget reservation,
  per-attempt idempotency, delivery classification with quarantine, keychain custody and the
  ledger stay in the app. The four verified `rig-core` facts (no carrier for delivery evidence and
  `StreamEnded` treated as retryable; no idempotency concept; a `rig/<version> (<OS> <arch>…)`
  fingerprint composed above our transport; 28 provider modules compiled in with no feature
  gates) each break one of those, or the generic `NodeCraft/<version>` User-Agent rule.
- **The gateway hardening the RFC asked for is mostly in place.** `x-litellm-response-cost` is
  read only from a successful reply (`LlmClient::chat_response` and the stream path call
  `error_for` first), so a gateway's `0` on a failure row is never read as free; a missing or
  unparsable header is `None`, and NaN, negative and absurd amounts are ignored (`usd`).
- **The browser settles the gateway-as-architecture question.** A hosted gateway can't reach
  `localhost` Ollama or a LAN ComfyUI, and a key can't live in a wasm bundle; the browser already
  calls endpoints directly with the page's own `fetch`.

### Answers to the RFC's open questions

1. **Which of #44–#48 ship?** All of them did, natively, so no dependency is needed for them.
2. **Which way would we rather be wrong?** About our own dialects drifting: that fails loudly
   (a parse error or a refused request in a fixture or live test), never as a silent behaviour
   change in a paid path.
3. **A single-publisher crate without a disclosure channel next to every key?** Not acceptable
   as a dependency. Vendoring or forking would make us its maintainers, which is option A with
   someone else's design.
4. **Is the catalog separable?** Yes, and it is the one part worth buying. The live OpenRouter
   and LiteLLM catalogs (#1075) are the first step: lists read from the endpoint at run time.
   Prices generated from a pinned snapshot are a follow-up: exact `Money`, never `f64` crossing
   into the catalog, a provenance line per row, and `catalog-check` rejecting a diff it can't
   explain.
5. **Does the browser settle it?** For gateways, yes (above). For client crates it adds a wasm
   build requirement that the candidates either fail or meet only without their transport.
6. **One layer for every consumer?** The agent, Text-node chat and the prompt helpers already
   share `LlmClient`; nothing about them argues for different answers.
7. **Is the honest answer "E, and write down why"?** For the client half, yes. This record is
   the "write down why".

### What would change it

- A provider whose wire shape is neither OpenAI- nor Anthropic-compatible and that we intend to
  ship natively: then build option C, a data-driven endpoint/wire split in `providers` (or
  upstream in craft-libs), not a dependency.
- A client crate that carries "never dispatched" evidence through its transport seam, has no
  retry defaults on a paid path, takes a host-supplied User-Agent, gates its providers by feature,
  builds for wasm32 over a host transport, and has a disclosure channel: then re-run the RFC's
  appendix experiment and revisit B.

### Follow-ups, if accepted

- A generated price snapshot for LLM rows (question 4), its own issue, after #1075 lands.
- Treat a reported cost of exactly `0` on a **successful** reply from a paid model as
  "not reported": LiteLLM writes `0` for models missing from its price map. Its own small issue,
  with a fixture.
- Close #489 with a link to this record once the owner accepts or amends it.
