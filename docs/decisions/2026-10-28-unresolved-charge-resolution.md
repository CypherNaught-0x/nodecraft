## 2026-10-28 · A person resolves an ordinary unresolved charge (#298)

A job that may have been billed but whose outcome NodeCraft can't see keeps its estimate in the
usage log as an **unresolved** charge: a lost submit, a 401/403/400 after the request left, a run
given up on after its retries, a restored job that can't be followed. None of these proves the
provider didn't bill it, so the charge keeps the estimate counted against the caps
([delivery evidence](../usage-accounting.md#delivery-evidence-and-strict-accounting)). Until now
nothing could ever take it out again: `usage.resolve_charge` (#332) covers only reconciled legacy
charges, whose problem is *which session* holds the money, not *whether* it was billed. This adds
the second path, for billing uncertainty, kept separate from the ownership one (#255, #257).

- **States.** A charge is offered for a decision only while it is `unresolved`, established
  (not quarantined, #255) and not one `usage.unsettled` lists (those stay on the #332 path, so no
  charge is on both). An `estimate` is a reservation a run still holds and is never offered; the
  run settles or releases it, or it becomes unresolved when the run gives up. `actual` charges
  are final. Unknown-price metadata holds carry no money and are not charges here.
- **Nothing is inferred.** No timer, error kind, HTTP status or retry count settles or releases
  one. The evidence is the person's own provider records, and the person states what they show:
  **settled** at the amount the provider billed (it becomes an `actual` charge at that amount,
  higher or lower than the estimate), or **released** because the provider never billed it
  (it leaves every total, like `UsageLog::release_confirmed` for #332). An optional short note
  records what they checked (an invoice line, a dashboard id).
- **Human only, token-bound, previewed.** `usage.unresolved` (read-only, agents may use it)
  lists the charges a page at a time, each with a token over the charge as reviewed: provider,
  provider instance, key, session, node, model, project, when it was recorded, amount, status and
  where it is kept. `usage.resolve_unresolved` (human only, in `HUMAN_ONLY`) carries that token;
  a charge that changed since is refused as stale, in the command against the snapshot and in the
  host service against the live log. `dry_run: true` returns the record and the session and
  project totals afterwards, changing nothing; the desktop enables Confirm only after a preview.
- **Never while a run might still settle it.** A run that retries or resumes under the same key
  settles the charge itself when it finishes, and a charge released under it would leave that
  completion with nothing to settle, which hides a real bill. So the run host refuses a decision
  while any run is waiting or running, while any restored job is being followed, and for a charge
  a parked, kept, saved or staged job still follows. The person finishes or stops runs first.
- **Never twice.** A decision moves the charge out of `unresolved`: settled is `actual` and
  final, released is gone from the log. A second confirmation of the same review finds nothing
  waiting and is refused with nothing changed. A released key a later job records again is a new
  charge, with a new token (its time and node differ), and is reviewed on its own.
- **Saved before kept.** The host applies the decision to a copy of the log, saves the copy in
  the run journal, then applies it to the live log in place, as for ownership decisions
  ([the ownership host decision](2026-10-20-ownership-host.md)). A save that fails (a full disk)
  leaves both as they were, and the same review can be confirmed again.
- **Budget effects.** Settling moves the session, overall and project totals by the difference
  between the estimate and the billed amount; releasing takes the estimate out of them. Nothing
  else changes: no run ceiling is credited (no run is going), and the admission view still adds
  the legacy group floors as before.
- **Audited.** Each decision is kept as an immutable `BillingResolution` record in the usage log
  (`billing_resolutions`): the charge's identity, session, node, model, project, when it was
  recorded, the amount it was open at, the outcome, the note and when it was decided. The newest
  `MAX_BILLING_RESOLUTIONS` (10,000) are kept, like the bounded usage list; a settled charge also
  stays in the list as an `actual` row. Loading checks each record's bounds, currency and status,
  not the charge's current state (it may have been recorded again). The field is written only
  when there are records, so a log without them stays readable by older NodeCraft; one with them
  is refused by an older reader (unknown field) rather than silently dropping the audit trail.
- **Known limits.** Hosts without a run host (the plain CLI MCP server) list nothing and can't
  decide. The desktop shows the first 50 waiting charges; each decision makes room for the next,
  and `usage.unresolved` pages through all of them. Release notes for this change wait for the
  next deliberate release.
