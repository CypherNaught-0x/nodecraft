## 2026-10-20 · The desktop saves an ownership decision before keeping it (#362, #384)

- **The run host owns the service.** `RunHost::register_ownership` provides
  `usage.reconcile_ownership` over the same shared usage log that paid jobs are charged to. The
  run journal (#508) already persists that log, so a decision needs no store of its own and
  survives a restart along with the charges.
- **Save first, then keep.** The decision is applied to a copy of the log. The copy is saved to
  the journal under the journal lock, in the same order the write-ahead uses, and only then
  replaces the live log. If the save fails (full disk, a broken journal), the live log and the
  saved file stay as they were, the person sees why, and the token they reviewed still holds for
  another try. An in-memory decision that a crash could later undo would be worse: someone could
  act on it in the meantime.
- **The snapshot follows on the next pump.** The service has no access to the session, so it
  marks the log as reconciled. The next `RunHost::pump`, or a host's explicit
  `RunHost::sync_usage`, replaces `UsageSnapshot` and `BudgetState`. The desktop panel calls
  `sync_usage` straight after confirming. A decision recorded outside the panel (another
  review) is picked up on the next frame; until then, a review of the changed group is refused as stale
  rather than shown wrongly.
- **Preview, then confirm, in the panel.** "Review quarantined charges" in the palette opens a
  panel. It lists the groups, then one group's evidence, totals and charges, 50 at a time.
  Confirm stays disabled until a dry run of the exact decision has been shown, and any change
  to the decision clears the preview. The panel doesn't offer staged decisions (`only`) yet: a
  group with more than 1,000 exceptions is refused by the command with its own message.
- **Pages fit the MCP budget.** `usage.ownership` pages are now at most 512 KiB of JSON, the
  same budget as MCP pages, down from 768 KiB. An agent therefore receives each page whole,
  and nothing relied on the larger size.
