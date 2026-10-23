## 2026-10-23 · Plugin providers run as catalog models (#174)

- **A template model is a catalog model.** A granted provider template's models join the
  generation catalog under the template's registered ID (`<registered id>/<model id>`), so the
  model picker, model swaps, quotes, budgets and the run journal treat them like built-in models.
  No new card kind, no plugin-specific run path.
- **Published with the grant, gone with it.** `Session::set_plugins` rebuilds the catalog from
  the built-in models plus the snapshot's plugin models and rebinds the generation kinds to it,
  together with the plugin action kinds; if either can't be published, neither is. A template
  that reaches an origin its grant didn't list, or places a credential without the secret
  capability, is never published.
- **The template says what it makes.** Templates gained an optional model `label` and `output`
  modality; text inputs are the body's `{text:ROLE}` placeholders, with `prompt` required. No
  parameters are exposed yet: a template's `{param:…}` members are left out of the request.
- **The key belongs to the package and the origin.** It lives in the keychain under
  `plugin.<package>:<API host>` and is set with the existing `keys.set`; an update that moves the
  origin needs the key again.
- **Revocation is checked at the last moment.** Each broker holds its provider's revoked flag and
  checks it right before the paid request, after authorisation; a withdrawn grant submits
  nothing new. A job already submitted is still polled and fetched, because it may be billed and
  dropping it would lose a paid result. The app also forgets its built adapters whenever the
  published providers change.
- **One package can't take the others down.** A package publishes at most 256 models, and one
  the catalog can't hold is left out alone rather than failing every plugin's publish.
- **Desktop only.** The app builds brokers on the public-addresses-only transport; the browser
  build refuses plugin providers, and the CLI has no plugin store.
- **Not yet:** a restarted app resuming a plugin job waiting on its provider, and settling parked
  jobs whose grant is gone, follow as the last part of #174.
