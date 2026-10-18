# 2026-10-18 · A fresh provider key after a reported failure (#223)

- **A provider that dedupes by idempotency key would hand a failed generation back on retry**, so
  after a provider reports a submitted job as finished-and-failed, `ProviderRunner` sends the next
  attempt under a derived key: `base#n`, where `n` counts reported failures for that provider and
  scheduler key. Keys stay ASCII and at most 64 bytes (`MAX_SENT_KEY`); a key that would not fit
  becomes an ASCII prefix plus an FNV-1a hash of the base.
- **Only a reported failure advances the count.** A lost reply, a cancel or an exhausted poll
  budget keeps the handle and resumes the same generation under the same key, so a retry still
  never charges twice.
- **The usage log charges each generation under the key its provider saw**, so two generations
  are two charges, as they are on the provider's bill.
- **One failed generation advances the count once:** each attempt remembers its generation, and
  a reported failure raises the count to `generation + 1`, so two runs that saw the same
  generation fail don't skip the next one.
- **The count survives a restart only through the host.** The update sink reports each attempt's
  `Sent { key, generation }`; a host that saves `Submitted` handles saves the generation with
  them and restores with `restore_generation`, which puts the handle back under its derived key
  and restores the count. A requeue under a new scheduler key, or `forget`, starts from zero.
