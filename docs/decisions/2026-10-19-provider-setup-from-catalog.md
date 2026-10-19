## 2026-10-19 · Provider adapters are built from the catalog in one place (#409)

`engine::provider_setup` is the only code where catalog provider ids meet adapter constructors.

- **One adapter per catalog provider, default instance.** `build` asks a maker once per provider
  the catalog names and binds what it returns into `ProviderRouting`; named instances (#275) come
  later from settings, not from the catalog.
- **Readiness instead of failure.** A provider without its key (`NeedsKey`), without a server
  (`NotSetUp`) or refused by the keychain (`Failed`) is recorded, not fatal: the rest still run.
  `ProviderSetup::select` refuses a node routed to it with an actionable message, before planning,
  so no request is sent and no budget is reserved.
- **Fixed keychain accounts.** `openai-images` → `openai`, `replicate` → `replicate`,
  `seedream` → `seedream`; ComfyUI uses `Account::server("comfyui", url)` and treats a stored key
  as an optional bearer token. Keys are never logged or put in a message.
- **The maker is a parameter.** The host passes `make` (keychain + shared HTTP client); tests pass
  mock providers, so they go through the same binding, readiness and selection.
- **Settings:** `[providers] comfyui_url` and `install_salt` (made once per install by the host).
