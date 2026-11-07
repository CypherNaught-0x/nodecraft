## 2026-11-06 · Test a key at its custom endpoint, and edit endpoints in the keys panel (#962)

This changes one rule of [custom base URLs](2026-11-03-custom-base-urls.md): `keys.test` used to
answer *unverifiable* and send nothing for an account set up for a custom base URL. It now
checks the key at that server, so **Test** in the keys panel matches what runs do.

- **Where the check goes.** The account's one base URL: `GET <base>/models` with
  `Authorization: Bearer` for `openai`, `GET <base>/models?limit=1` with `x-api-key` and
  `anthropic-version` for `anthropic` (`key_test::check_at`). Redirects are not followed, the key
  is a secret header, and a rejection names "the custom server", not the provider.
- **Only where a run would go.** Every settings view the check host gives
  (`key_test::redirected_to`) must redirect the account to the same usable base URL, and every
  provider sharing the account's key must agree (`custom_endpoints::account_base_url`).
  Otherwise the answer stays *unverifiable* and nothing is sent; and once any view redirects the
  account, the key never goes to the provider's own API.
- **Desktop views.** The settings the app last saved (first those loaded at startup, then each
  save through the keys panel) and the store's own file as it is now. A hand edit naming another server than
  the app saved therefore refuses the check until a restart, as the agent panel does.
- **Editing.** The desktop keys panel's **Endpoints** rows show each provider's address in use
  and set, change or reset it. An address is checked (`custom_endpoints::check`) before anything
  is saved, and the settings file is written at once, so the file view agrees with the saved one.
  A reset writes a blank entry (meaning the provider's own API) rather than removing it, since
  settings keep entries they don't model as written. For the same reason an entry spelled
  differently (`OpenAI`) can't be removed from the panel: its row shows the address can't be used,
  and saving says to fix `config.toml`. The browser keeps its Providers panel field.
