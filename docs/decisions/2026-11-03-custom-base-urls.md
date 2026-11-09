## 2026-11-03 · Custom base URLs per provider: URL rules and cost (#960)

A person can point a catalog provider at another server that speaks its API: a LiteLLM proxy,
vLLM, LM Studio or a gateway. This record covers:

- which addresses are accepted;
- where they are kept;
- where the key may go;
- what a run through one costs.

The cost part applies the
[custom endpoint cost rule](https://github.com/CypherNaught-0x/nodecraft/pull/1026) proposed for
#961, in its minimal, conservative form. See "Not done" below.

### Where and for which providers

- **Per provider, in settings only.** `[providers.base_urls]` maps a catalog provider id to a
  base URL that includes the API's version path, such as `openai = "http://127.0.0.1:4000/v1"`.
  - The browser keeps the same map with its per-site server addresses (`providers.settings_set`,
    the Providers panel).
  - The URL is never stored in a project, a key or the keychain. The key stays in the
    provider's own keychain account.
  - The desktop app reads the table at startup. An edit to `config.toml` takes effect after a
    restart. `keys.test`, the agent panel and the CLI read the file when they run.
- **The list is data.** `engine::custom_endpoints::PROVIDERS` names each provider, its label, its
  default, and whether the browser can use it.
  - It holds `openai` (chat), `anthropic` (chat) and `openai-images`.
  - The command schema and the browser panel's fields are generated from it.
  - Adding a provider means adding a row and giving its adapter a `with_base_url`.
- **Nothing set, nothing changes.** With no entry, or a blank one, every adapter keeps its own
  API, its retry policy and its cost behaviour exactly as before. That includes the LiteLLM
  profile (see "Not done").

### URL rules

`providers::base_url::BaseUrl::parse` is the only check, and it runs before any key is read.

- **HTTPS** is accepted for any host.
- **Plain HTTP only for a local host.** "Local" is a positive list:
  - `127.0.0.0/8` and `::1`;
  - the RFC 1918 ranges;
  - IPv4 link-local `169.254.0.0/16` and IPv6 link-local `fe80::/10`;
  - unique local `fc00::/7`;
  - the IPv4-mapped forms of the IPv4 ranges above;
  - exactly `localhost`.
- **Everything else is remote and needs HTTPS:**
  - carrier-grade NAT `100.64.0.0/10`. Its traffic crosses the ISP's network. Tailscale users
    either use HTTPS (MagicDNS certificates) or keep the server on a private address;
  - Teredo, 6to4 and NAT64, which are never unwrapped because they travel over public relays;
  - the 6to4 relay range `192.88.99.0/24`;
  - `240.0.0.0/4`, multicast, unspecified, benchmarking and documentation addresses;
  - every host name but `localhost`, including `.localhost` subdomains and names that resolve
    to private addresses. DNS records can change, and the browser can't resolve names at all.
  - This list is narrower than the SSRF check `providers::http::public_address` ("not public").
    #961's "free (local)" choice should use this list too.
- **Refused:** every other scheme (`ftp`, `file`, `ws`, `javascript`, …), no host, a user name or
  password, a query, a fragment, control characters, and anything over 2,048 bytes.
- **No echo.** Every refusal is one fixed message that never quotes the input, since a pasted
  URL can hold a key. The panic hunt fails errors that echo their input.
- **No fallback.** Each of these refuses its adapter before planning, with the reason:
  - an entry that is set but can't be used;
  - an entry that isn't text (kept as written, so a save writes it back). TOML `nan` and `inf`
    read as null and are kept as `false`;
  - a key spelled like a provider's with different case or `_` (`OpenAI`, `openai_images`);
  - a `base_urls` value that isn't a table, or a table of more than 64 entries (marked with `*`,
    which refuses every listed provider).
  - NodeCraft never sends the key to the default host instead.
  - Other unknown keys are kept and ignored.
- **Browser storage.** A stored base URL that no longer passes is kept and refuses at build time.
  A damaged record that held `base_urls` refuses every listed provider until the next save.
- **Not covered: a whole-file or whole-section reset.** When the native settings file can't be
  read, or its whole `[providers]` section is invalid, `craft-settings` uses defaults and
  reports it at startup. That brings back the providers' own APIs. It is the existing rule for
  every setting, and this change leaves it as it is.

### Where the key goes

The adapter builds every request URL from the base URL. The key is a secret header, and the
native transport drops it when a redirect leaves the origin (`HttpRequest::redirected`). OpenAI
Images sends without following redirects.

Several adapters read one keychain account: `openai`, `openai-images` and `openai-audio` all read
`openai`. While any provider that reads an account has a base URL entry, that key never goes to a
provider's own API:

- **Other adapters.** An adapter reading that account with no entry of its own is refused
  (`custom_endpoints::shared_key_conflict`). `openai-audio` takes no base URL, so it stays
  refused until the entry is removed.
- **`keys.test`.** It answers *unverifiable* for that account and sends nothing
  (`custom_endpoints::account_redirected`). On the desktop it checks both the settings loaded at
  startup and the file as it is now, and either is enough. So an unreadable file never lowers
  protection below what was loaded.
- **The desktop agent panel.** It follows the OpenAI or Anthropic base URL with the
  custom-endpoint client (no spend header, no resend), or refuses, under the same shared-key
  rule. Both views must allow the request and name the same base URL. If they differ, it asks
  for a restart.
- **The browser.** Its `fetch` follows redirects itself and drops only `Authorization` across
  origins. So in the browser only OpenAI chat (bearer key) takes a base URL. Anthropic sends
  `x-api-key`, which a redirect would carry to another host. OpenAI Images needs a no-redirect
  transport the browser lacks. reqwest's wasm client doesn't expose `fetch`'s
  `redirect: "error"`, so these two are refused in the browser. They are hidden from the panel
  and refused when saved or built.

The model's catalog identity is unchanged: its provider id (scheduler lane, routing target) and
the `provider_model` sent on the wire.

### Cost

- **Catalog prices are kept.** A catalog model routed to a custom base URL keeps its catalog
  identity and price (#961, "Scope").
- **Never free.** `custom_endpoints::apply` runs for the shown quote and for the admitted run. A
  step routed to an adapter with a custom base URL that would be quoted free, or at exactly zero,
  becomes unknown-price (`Unpriced::Invalid(..)`), the path ComfyUI partner nodes use (#833).
  - Under any cap the step is blocked (`Block::Unknown`), and there is no per-run override.
  - Without a cap, the run needs the explicit partial-price consent (`partial` plus
    `unpriced_jobs`), and the per-run ceiling bounds its unknown-price jobs.
  - No shipped model on these three providers is free today, so this guards catalogs that add
    one.
- **Bound into the quote.** Each routing binding captures a digest of its adapter's base URL when
  the binding is built (`Provider::custom_base_url`). `custom_endpoints::bind` puts the digests
  of the selected targets into the quote token. A quote taken before the URL changed can't start
  the run, and the routing nonce changes too.
- **Settlement never below the estimate.** A custom server's token counts can't be checked. A job
  on a custom endpoint is settled at the larger of its estimate and its measured usage at catalog
  rates (`jobs::settled`). A reply that reports zero tokens can't hand budget back. Its
  `x-litellm-response-cost` header isn't read.
- **Retries.** A custom base URL is assumed to ignore idempotency keys. Every `LlmClient` with one
  returns `retry_ambiguous_submit() == false`, so a lost reply or an error reply is never resent
  under the same key. The reservation closes as `Charge::Unresolved` at its estimate, for
  reconciliation. OpenAI Images was already terminal. A retry the person asks for is a new job
  under a new key, with a new reservation.

### Not done

These parts of the #961 rule need settings and UI that don't exist yet. They apply to models the
catalog doesn't price, which is the LiteLLM catalog work (#489):

- the per-endpoint cost rule (*unpriced*, *priced by you*, *free (local)*);
- tracking free jobs, and withdrawing the free mark when a cost is reported;
- the "Trust LiteLLM's reported cost" toggle;
- the API keys and Budgets panel rows;
- the #961 changes to the existing LiteLLM profile: terminal ambiguous submits, a reported `0`
  read as no report, and the spend header read only from a trusted LiteLLM endpoint.

This change leaves the LiteLLM profile as it was, so nothing changes for anyone without a base
URL. Until that work lands, a custom endpoint is never free, and nothing can mark it free.
