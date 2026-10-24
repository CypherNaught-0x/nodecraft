## 2026-10-20 · A person settles or releases a reconciled charge (#332)

- **Nothing guesses the outcome.** A legacy charge whose ownership was reconciled (#325) is an
  established open charge again, but the job that held it ended in an older NodeCraft, so no
  run will settle it. Settling it at its estimate, or releasing it on a timer, would invent a
  fact about the provider's bill. A person who has checked their provider's records says what
  they show: `settled` at the billed amount, or `released` because it was never billed.
- **Releasing an unresolved charge is allowed here.** The automatic release path refuses an
  unresolved charge, because nothing confirmed it wasn't billed. A person's statement is
  exactly that confirmation, so `UsageLog::release_confirmed` allows it, and only this path
  calls it.
- **Bound to what was reviewed.** `usage.unsettled` returns a token over the charge as the
  person saw it: provider, key, session, amount, status and where it is kept. A decision with
  a stale token is refused, both in the command (against the snapshot) and in the host service
  (against the live log).
- **Final, and audited.** A decision is kept as an immutable `ChargeResolution` record
  (usage-log marker 4), at most one per reconciled charge, so the records are bounded by the
  reconciliations. Loading checks each record names a reconciled charge in its currency. It
  doesn't check the charge is still open: a released key may be recorded again by a new job.
- **Saved before kept, like ownership decisions.** The run host tries the decision on a copy,
  saves the copy in the run journal, then applies it to the live log in place (see
  [the ownership host decision](2026-10-20-ownership-host.md)). The host also refuses a charge
  a saved run still follows; that run settles it when it resumes. The host now seeds the
  journal's list of followed runs when it opens the journal, so a decision kept before the
  first save no longer drops the parked runs from the saved journal.
- **Known limits.** Loading doesn't check a resolution against the charge's current state, so a
  hand-edited file that marks a still-open charge resolved is admitted. The charge then stays
  open, counted in the totals but no longer offered for a decision: conservative, never
  under-counted. A reconciled key that a saved run settles and then retries under the same key
  is listed again, though the host refuses to decide it while that run follows it. The desktop
  shows the first 50 waiting charges; each decision makes room for the next, and
  `usage.unsettled` pages through all of them.
- **Human only.** Agents can list the charges (`usage.unsettled`) but not decide them, as with
  `usage.reconcile_ownership`.
