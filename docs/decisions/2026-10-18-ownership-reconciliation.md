## 2026-10-18 · Person-confirmed reconciliation of quarantined charge ownership (#325)

- **A person decides, never the code.** Quarantined legacy charges (#257) stay quarantined
  until someone who has checked their own provider records states, for every charge of one
  (session, currency) group, whether its money sits in the named session's total or in the
  folded earlier-sessions total. Nothing infers it from amounts, timestamps or coverage.
- **Bounded decisions, staged when large.** A decision covers every charge still quarantined in
  a group (one default holder plus at most 1,000 exceptions) or, with `only`, just the listed
  charges (at most 1,000): one stage. A balanced split of a group larger than the exception
  limit is decided stage by stage, each against a fresh review token. Every stage is its own
  immutable record carrying the group's evidence unchanged; the group stays quarantined until
  its last stage. Caps are cumulative: named-held charges across a group's stages fit the named
  total at import, earlier-held ones across every stage of the currency fit what the earlier
  total held at import. Load checks conservation exactly: a group's staged charges plus its
  still-quarantined ones equal its open amount at import (quarantined charges can't settle),
  each charge once, and a closed group's stages end in one complete record.
- **Bound to what was reviewed.** `usage.ownership` returns a token over the group's evidence,
  charges (identity, amount, status, location), the named, earlier and established-open totals
  the number of this group's stages and how much the earlier total can still give (which also moves when
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
- **Host contract (a foundation, not yet a feature).** The host registers
  `ownership_cmds::ownership_service` over its live log; after a success it saves the log
  durably and replaces the `UsageSnapshot` resource, or later reviews are refused as stale. The
  in-memory change succeeds before the save, so until a host makes the save part of the
  success boundary (#384), a failed save can lose a confirmed decision; nothing is double
  counted, the group is simply quarantined again on the next load. The human-confirmation step
  is the host's too: `HUMAN_ONLY` keeps agents out, and a host must show the dry-run result
  and take the person's confirmation before dispatching. No app host registers the service yet
  (#384), so #325 stays open until one does.
- **Not a settlement decision.** Reconciling ownership says where money sits; it doesn't
  settle or release charges whose provider outcome is unknown. That monetary decision (#332)
  is separate.
- **Human only.** Agents may list groups (`usage.ownership` shows what `usage.export_csv` already
  does) but `usage.reconcile_ownership` is in `HUMAN_ONLY`.
