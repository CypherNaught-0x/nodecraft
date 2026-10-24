## 2026-10-22 · Plugin cards record the package they were made with (#173)

- **The lock lives in the card.** A plugin card keeps the package ID, version, digest and action
  label it was made with in its node's config (`plugin.package`, `plugin.version`,
  `plugin.digest`, `plugin.label`). Config is kind-owned and kept as-is, so the record survives a
  save, a reopen, and a session where the plugin isn't there. A project file can write anything
  there, so a lock counts only on a kind of its own package, with a canonical version and a label
  as a definition has one; anything else is ignored.
- **A missing plugin leaves a labelled card.** Without its kind the card keeps its settings, ports
  and wires, shows its action's label, and says which package and version it needs. A run says
  its plugin isn't installed, granted or enabled here; in the browser, that plugins can't run
  there yet.
- **The grant decides what runs, not the lock.** A card runs the active, granted version of its
  package. When ports or settings no longer match, the card offers "Update card" (`node.migrate`):
  the new ports, lock and settings, and the removal of wires to dropped ports, in one undo step.
  The window lists what it dropped afterwards, and undo puts it back; agents read the same
  warnings with `dry_run` first. The card offers it whenever its lock, ports or settings differ
  from what the kind would make now, so its lock names the version it runs.
- **Uninstall and prune keep what the open project uses.** Both send the digests of the open
  project's cards to the store, which keeps them (at most 4096, so a hostile file can't block
  either). Projects that aren't open aren't seen; a card in
  one keeps its lock and becomes a labelled card until the package is installed again.
