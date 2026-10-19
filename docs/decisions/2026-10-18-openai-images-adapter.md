## 2026-10-18 · OpenAI Images uses unknown usage prices and refuses ambiguous automatic replay (#13)

The standalone adapter supports complete generate/edit/reference/mask/options/N behavior with two
explicit dated GPT Image 2.5 models. A currency-only ByUsage catalog rule returns unknown rather
than fabricating an amount from an output-only rate. Finite caps refuse unknown paid registrations;
uncapped command callers must explicitly accept partial pricing. Raw provider/runner callers own
host authorization and paid classification. Token usage is not a monetary invoice; a missing amount
may leave no money ledger row and never establishes zero spend. This does not implement a paid
desktop host or change pending accounting/descriptor/generation follow-ups.

Http::send_no_redirect is an opt-in operation. Its default invokes no legacy transport; native
Reqwest sends one hop; builtin wasm refuses before fetch. Secret-header stripping alone cannot
protect image bodies on redirects. Images inherits conservative submission uncertainty and opts
out of automatic uncertain no-handle retry. The captured provider decides the same retry predicate
used for charge retention and scheduler outcome. Diagnostic kinds, Cancelled, handles and proved
no-send retries retain their meanings. No header/local key asserts remote idempotency, and manual
rerun, custom clients and cold restart remain explicit host recovery/consent responsibilities.

Bounded borrowed admission precedes multipart/JSON copies, owned JSON unescaping and base64
payload allocation. Nonempty partial N is retained without a fill request; a late-invalid batch
publishes no Completed. See [Providers](../providers.md) for exact limits and transport/platform qualifications.
