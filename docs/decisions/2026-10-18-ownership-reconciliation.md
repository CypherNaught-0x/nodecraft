## 2026-10-18 · Person-confirmed reconciliation of quarantined charge ownership (#325)

- **A person decides, never the code.** Quarantined legacy charges (#257) stay quarantined
  until someone who has checked their own provider records states, for every charge of one
  (session, currency) group, whether its money sits in the named session's total or in the
  folded earlier-sessions total. Nothing infers it from amounts, timestamps or coverage.
- **Whole groups, bounded decisions.** A decision covers every charge of a group: one default
  holder plus at most 1,000 exceptions. The group's evidence invariant (its open sum equals the
  retained uncertain charges) never has to describe a half-reconciled group.
- **Bound to what was reviewed.** `usage.ownership` returns a token over the group's evidence,
  charges (identity, amount, status, location), the named, earlier and established-open totals
  the reconciliation count and how much the earlier total can still give (which also moves when
  another session's listed charges change, so such a change makes a review stale). `usage.reconcile_ownership` is refused when the token no
  longer matches; the host service checks it again against the live log.
- **Refused when the import evidence contradicts it.** Named-held charges must fit within the
  named total recorded at import; earlier-held charges must fit within what the earlier total
  held at import (summed across reconciliations of the currency) and what it holds now, and
  must leave enough for the listed charges of folded sessions that the earlier total covers (as
  load admission requires); the named total must still cover its open charges; evicted charges
  need room in pending.
- **Dry run first.** `dry_run: true` applies every check to the host's snapshot and returns the
  record and the named and earlier totals the decision would produce, changing nothing.
- **Accounting consequence.** Earlier-held money moves to the named session; the project total
  is unchanged. The charges become established and can then be settled or released through the
  normal paths. Reconciliation settles, releases or refunds nothing, and conservative session
  admission for the group ends because the person resolved what it guarded against.
- **Audit and wire.** Each decision is saved as an immutable record holding the group's original
  evidence, every charge with its amount and holder, the moved amount and the host's time. Logs
  with records use ownership marker 3, so older builds refuse them with "use a newer NodeCraft"
  instead of rejecting the moved earlier money as corruption. Logs without records keep marker
  1 or 2 and are saved byte for byte as before. Marker 3 is permanent once a record exists:
  records are never removed.
- **Host contract.** The host registers `ownership_cmds::ownership_service` over its live log;
  after a success it saves the log and replaces the `UsageSnapshot` resource, or later reviews
  are refused as stale. No app host registers it yet (the desktop run host is #201).
- **Human only.** Agents may list groups (`usage.ownership` shows what `usage.export_csv` already
  does) but `usage.reconcile_ownership` is in `HUMAN_ONLY`.
