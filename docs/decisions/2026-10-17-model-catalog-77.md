# 2026-10-17 · Model catalog (#77)

- **Catalog files are TOML** (`assets/catalog/<provider>.toml`), embedded in the build so they
  work offline and on the web, parsed with `toml` (parse + serde only). Comments and nested
  tables read better than JSON for hand-edited data.
- **The shipped catalog prices nothing yet.** It holds free local ComfyUI-backed models. Priced
  cloud entries land with their provider adapters, whose PRs say where the prices came from:
  a guessed price would mislead people about what a run costs.
- **Prompts are an inline input role.** A media node's own prompt field fills `inline` text
  roles, so a fresh node with nothing wired still offers text-to-image models; a wired text
  input replaces the inline prompt.
- **Quotes are computed from resolved parameters** (schema defaults applied) and round up, and a
  per-unit model has exactly one counted parameter; anything else is rejected when the catalog
  loads rather than under-quoted later.
- **`catalog-check` is an integration test** of `nodecraft-catalog` that `cargo xtask
  catalog-check` runs, so xtask stays std-only and `cargo test` covers it too.
