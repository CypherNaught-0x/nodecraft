## 2026-10-22 · Plugin cards record the package they were made with (#173)

- **The lock lives in the card.** A plugin card keeps the package ID, version, digest and action
  label it was made with in its node's config (`plugin.package`, `plugin.version`,
  `plugin.digest`, `plugin.label`). Config is kind-owned and kept as-is, so the record survives a
  save, a reopen, and a session where the plugin isn't there.
- **A missing plugin leaves a labelled card.** Without its kind the card keeps its settings, ports
  and wires, shows its action's label, and says which package and version it needs. A run says
  its plugin isn't installed, granted and enabled here; in the browser, that plugins can't run
  there yet.
- **The grant decides what runs, not the lock.** A card runs the active, granted version of its
  package. When ports or settings no longer match, the card offers "Update card" (`node.migrate`):
  the new ports, lock and settings, and the removal of wires to dropped ports, in one undo step.
  The window lists what it dropped afterwards, and undo puts it back; agents read the same
  warnings with `dry_run` first. A card whose ports still match isn't asked to update, so its lock
  can name an older digest.
- **Uninstall and prune keep what the open project uses.** Both send the digests of the open
  project's cards to the store, which keeps them. Projects that aren't open aren't seen; a card in
  one keeps its lock and becomes a labelled card until the package is installed again.
