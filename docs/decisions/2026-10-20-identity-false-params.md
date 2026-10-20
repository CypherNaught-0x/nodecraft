## 2026-10-20 · `identity: false` parameters are sent but not hashed (#162)

- **Sent, not hashed.** A parameter whose contract rule declares `identity: false` stays in what
  a job receives (`ResolvedParams::visible`) and is listed in `ResolvedParams::unhashed`; a
  result's input hash covers `identity_values()`, the visible values less those. Editing it keeps
  results fresh, which is what the declaration means (for example a note or a provider-side
  label that cannot change the output).
- **One flag, one meaning.** The catalog generation kind already left such parameters out
  through `into_identity`; derived contracts now behave the same, so the `affects.identity`
  flag no longer depends on which resolution path a kind uses.
- **Custom hooks opt in.** A kind with its own `validate_instance` builds `ResolvedParams`
  itself; an empty `unhashed` keeps today's behaviour (everything visible is hashed), which can
  only make a result stale too often, never too rarely.
