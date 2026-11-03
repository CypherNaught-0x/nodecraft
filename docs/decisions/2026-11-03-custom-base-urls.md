## 2026-11-03 · Custom base URLs per provider: URL rules and cost (#960)

A person can point a catalog provider at another server that speaks its API: a LiteLLM proxy,
vLLM, LM Studio or a gateway. This records which addresses are accepted, where they are kept,
and what a run through one costs. The cost part applies the
[custom endpoint cost rule](https://github.com/CypherNaught-0x/nodecraft/pull/1026) proposed for
#961 in its minimal, conservative form; see "Not done" below.

### Where and for which providers

- **Per provider, in settings only.** `[providers.base_urls]` maps a catalog provider id to a
  base URL that includes the API's version path, such as `openai = "http://127.0.0.1:4000/v1"`.
  The browser keeps the same map with its per-site server addresses (`providers.settings_set`,
  the Providers panel). The URL is never stored in a project, a key or the keychain. The key
  stays in the provider's own keychain account.
- **The list is data.** `engine::custom_endpoints::PROVIDERS` names each provider, its label and
  its default. The command schema and the browser panel's fields are generated from it. It now
  holds `openai` (chat), `anthropic` (chat) and `openai-images`. Adding a provider means adding
  a row and giving its adapter a `with_base_url`.
- **Nothing set, nothing changes.** A missing or blank entry keeps the provider's own API, and
  its retry and cost behaviour stay exactly as they were. A hand-edited entry that isn't text,
  or names a provider not in the list, is dropped when settings are read. Dropping it does not
  reset `[providers]`.

### URL rules

`providers::base_url::BaseUrl::parse` is the only check, and it runs before any key is read:

- **HTTPS** to any host.
- **Plain HTTP only for a local host**: an IP literal that `providers::http::public_address`
  says isn't public (loopback, RFC 1918, carrier NAT, link-local, IPv6 ULA and link-local,
  and mapped forms of these), or exactly `localhost`. Any other name counts as remote even if
  it resolves to a private address today: its records can change, and the browser can't
  resolve names. Names under `.localhost` are refused for the same reason. To use HTTP to a
  LAN box, use its IP address. This is the same classification #961 uses.
- **Refused**: every other scheme (`ftp`, `file`, `ws`, `javascript`, …), no host, a user name
  or password, a query, a fragment, control characters, and anything over 2,048 bytes.
- **No echo.** Every refusal is one fixed message that never quotes the input, since a pasted
  URL can hold a key. The panic hunt fails errors that echo their input.
- **No fallback.** If a base URL is set but can't be used, the adapter isn't built. The run is
  refused before planning, with the reason. NodeCraft never sends the key to the default host
  instead.

### Where the key goes

The adapter builds every request URL from the base URL, so the key goes to that host only. It
is a secret header, dropped when a redirect leaves the origin (`HttpRequest::redirected`). OpenAI
Images already sends without following redirects. The model's catalog identity is unchanged:
the provider id (scheduler lane, routing target) and the `provider_model` sent on the wire.
`keys.test` still checks a key against the provider's own API. It is the person's explicit check
of that provider's key, and it does not read the base URL.

### Cost

- **Catalog prices are kept.** A catalog model routed to a custom base URL keeps its catalog
  identity and price (#961, "Scope"). The quote, budgets and settlement at the catalog's token
  rates work as before.
- **Never free.** If a step routed to an adapter with a custom base URL would be quoted free,
  or at exactly zero, `custom_endpoints::apply` makes it unknown-price with
  `Unpriced::Invalid(..)`. It runs for the shown quote and for the admitted run. That is the
  path ComfyUI partner nodes and the hosted ComfyUI profile use (#833). Under any cap the step
  is blocked (`Block::Unknown`). Without a cap, the run needs the explicit partial-price consent
  (`partial` plus `unpriced_jobs`), and the per-run ceiling bounds its unknown-price jobs. There
  is no per-run override under a cap. No shipped model on these three providers is free today,
  so this is a guard for catalogs that add one.
- **Bound into the quote.** Each routing binding captures a digest of the adapter's base URL
  when it is built (`Provider::custom_base_url`). `custom_endpoints::bind` puts the digests of
  the selected targets into the quote token, so a quote taken before the URL changed can't
  start the run. The routing nonce changes too.
- **Retries.** A server the person points at is assumed to ignore idempotency keys. Every
  `LlmClient` with a custom base URL, and the LiteLLM profile, returns
  `retry_ambiguous_submit() == false`. A lost reply or an error reply is never sent again under
  the same key. The reservation closes as `Charge::Unresolved` at its estimate, so it can be
  reconciled. OpenAI Images was already terminal. A retry the person asks for is a new job
  under a new key, with a new reservation.
- **Reported spend.** `x-litellm-response-cost` is read only from the LiteLLM profile. Any
  server can send that header, so it is never read from an OpenAI or Anthropic endpoint, custom
  or not. A catalog price is never lowered by what a custom server reports. A reported `0`
  counts as no report, because LiteLLM writes `0` when it has no price for a model.

### Not done

These parts of the #961 rule need settings and UI that don't exist yet. They apply to models the
catalog doesn't price, which is the LiteLLM catalog work in #489:

- the per-endpoint cost rule (*unpriced*, *priced by you*, *free (local)*);
- free-job tracking and withdrawing a free mark when the endpoint reports a cost;
- the "Trust LiteLLM's reported cost" toggle;
- the API keys and Budgets panel rows.

Until that work lands, a custom endpoint is never free, and nothing can mark it free.
