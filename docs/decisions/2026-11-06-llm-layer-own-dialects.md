## 2026-11-06 · LLM calls keep our own dialects; gateways stay endpoints; catalogs may become data (#489)

Status: proposed. #489 asks for a decision record, not code, and that decision is the owner's to
take. This record proposes one, answers the RFC's open questions from the code as it is today,
and lists what would change it. Nothing here changes behaviour.

### Proposal

**Options A + D from the RFC, with the catalog half of D as the only new work:**

- **A, without the restructure for now.** Keep hand-written dialects, `Dialect::OpenAi` and
  `Dialect::Anthropic` in `crates/providers/src/llm/`; do the RFC's restructure (a data-driven
  endpoint/wire split) only when a third wire shape is actually needed.
- **D. A gateway stays one endpoint** (`Endpoint::litellm`, custom base URLs #960), never the
  architecture and never operated by the app.
- **Catalogs as data:** model lists and prices may be generated from a pinned, provenance-stamped
  upstream snapshot into `assets/catalog`, checked by `cargo xtask catalog-check`, without taking
  anyone's client code.
- **B (depend on a client crate) is declined** while any of the four guarantee failures the RFC
  verified in `rig-core` 0.45.0 stands. **C** (clean-room the shape) is what A becomes the day a
  third dialect is needed; it isn't started speculatively. **E**'s trigger (someone claiming
  #44–#48) has already fired, so "decide nothing" is no longer the honest answer.

### Why, from the code as it is now

- **Most of the long tail shipped without a layer.** The RFC's strongest argument for a layer was
  the queue of adapters #44–#48 (native Gemini, Azure OpenAI, Bedrock, Vertex, Hugging Face) and
  the agent (#114) needing a second backend. All six are closed. #44, #45, #46 and #48 shipped as
  OpenAI-compatible presets of under 150 lines each (`gemini_llm/`, `huggingface/`,
  `llm/azure.rs`, `llm/bedrock.rs`) over the existing OpenAI dialect; #47 shipped Vertex *image*
  models (`google_cloud/gemini_images`), not Vertex chat. `llm/` is 2,661 lines with its tests. So
  the adapters a layer would have bought were small, inside the money-safety contract and free of
  upgrade tax, with two exceptions: native Bedrock (SigV4 and the Converse API, which
  `llm/bedrock.rs` defers to this decision) and Vertex chat were not built.
- **The line that doesn't move is still the deciding fact.** Quote before pay, budget reservation,
  per-attempt idempotency, delivery classification with quarantine, keychain custody and the
  ledger stay in the app. The four verified `rig-core` facts (no carrier for delivery evidence and
  `StreamEnded` treated as retryable; no idempotency concept; a `rig/<version> (<OS> <arch>…)`
  fingerprint composed above our transport; 28 provider modules compiled in with no feature
  gates) each break one of those, or the generic `NodeCraft/<version>` User-Agent rule.
- **One item of D's hardening holds today; the rest is follow-up.** `x-litellm-response-cost` is
  parsed before `error_for` but kept only for a successful reply (`LlmClient::chat_response` and
  the stream path drop it on error), so a gateway's `0` on a failure row is never read as free; a
  missing or unparsable header is `None`, and NaN, negative and absurd amounts are ignored
  (`usd`). The value is discarded on failure rather than quarantined and reconciled, and the
  header isn't read on custom base URLs (#960). D's other items (reconciling against LiteLLM's
  `/key/info` and `x-litellm-call-id`, documenting that gateway-side retries must be off for paid
  runs, refusing an AGPL gateway, a data-handling line per endpoint) don't exist yet; they are
  listed below.
- **The browser settles the gateway-as-architecture question.** A hosted gateway can't reach
  `localhost` Ollama or a LAN ComfyUI, and a key can't live in a wasm bundle; the browser already
  calls endpoints directly with the page's own `fetch`.

### Answers to the RFC's open questions

1. **Which of #44–#48 ship?** All five closed, four of them through the OpenAI dialect. Native
   Bedrock (SigV4/Converse) and Vertex chat are not built, and this record declines them for
   now: Bedrock and Vertex models stay reachable through their OpenAI-compatible surfaces or a
   LiteLLM endpoint. Shipping either natively is the "third wire shape" that would trigger C.
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
   build requirement: `rig-core` meets it only without its `reqwest` transport, and `liter-llm`'s
   budget and idempotency layer doesn't build for wasm (both as the RFC reports).
6. **One layer for every consumer?** The agent and Text-node chat already use the same `llm`
   types and client; prompt helpers and auto-select would go through it too. Nothing about the
   four consumers argues for different answers.
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

The RFC's appendix experiment (rig's HTTP client over our `Http`, in a browser, through five
failure paths) was not run for this record; it is the evidence to gather before revisiting B.

### Follow-ups, if accepted

- A generated price snapshot for LLM rows (question 4), its own issue, after #1075 lands.
- Treat a reported cost of exactly `0` on a **successful** reply from a paid model as
  "not reported": LiteLLM reportedly reports `0` (or nothing) for models missing from its price
  map. Its own small issue, which confirms that against LiteLLM's source and pins it with a
  fixture.
- The rest of D's hardening: reconcile a LiteLLM charge by `x-litellm-call-id` against
  `/key/info`, read the cost header on custom base URLs where it is LiteLLM, document that
  gateway-side retries must be off for paid runs, and a data-handling line per endpoint.
- Update the `llm/bedrock.rs` note that defers SigV4/Converse to this decision.
- Close #489 with a link to this record once the owner accepts or amends it.
