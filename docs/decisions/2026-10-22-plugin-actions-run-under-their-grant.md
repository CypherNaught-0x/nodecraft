## 2026-10-22 · Granted plugin actions run as cards, and stop with their grant (#173)

- **The worker publishes what may run.** After every plugin operation, and on a refresh when the
  window opens, the plugin worker publishes a snapshot of the actions of every enabled package
  holding a live grant for its active digest. The window swaps it into the session between frames;
  a changed snapshot only swaps the `plugin.` node kinds, so built-in kinds never move.
- **Each action carries its grant.** A published action holds the grant id, package, digest and
  epoch it was published under. A new snapshot keeps the very same action when that stamp is
  unchanged, so jobs already running hold it; every other old action is flagged revoked.
- **Revoked means dropped.** A revoked action stops evaluating at its next fuel check, refuses to
  start, and any result that still arrives is dropped by the run host and by live evaluation
  rather than recorded on the card. Revocation takes effect when the window next swaps in a
  snapshot, a frame after the worker saves it; a result recorded in between was computed under a
  grant that was still live when it ran.
- **A changed set looks again.** Swapping in a different set makes live evaluation look at the
  canvas afresh and retry plugin cards that failed, since a card's input hash doesn't cover its
  package: a project opened before the worker published, or a package updated, re-runs its cards.
- **Every card registers.** A definition is compiled into its card, checked as the registry
  checks node kinds, when it is granted and again when enabled; if a set still can't be
  registered, none of its actions run until the next one, so cards and actions never disagree.
- **Plugin results are never cached.** A card's input hash doesn't cover the package that ran it,
  so a reused result could outlive an update; plugin kinds always run.
- **Lists only last, and one input at least.** A text list input takes every remaining connection,
  so only a definition's last input may be a list; and a card runs once its inputs are connected,
  so an action with none is refused. Both are refused when the package is granted or enabled.
- **Off the runtime's threads.** Plugin actions are bounded by fuel but can take a second or two,
  so they run as long-running jobs.
