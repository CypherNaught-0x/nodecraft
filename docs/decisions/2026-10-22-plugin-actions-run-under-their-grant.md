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
  rather than recorded on the card.
- **Plugin results are never cached.** A card's input hash doesn't cover the package that ran it,
  so a reused result could outlive an update; plugin kinds always run.
- **Lists only last.** A text list input takes every remaining connection, so a definition may give
  only its last input the list kind; one that doesn't is refused when it is granted or enabled.
- **Off the runtime's threads.** Plugin actions are bounded by fuel but can take a second or two,
  so they run as long-running jobs.
