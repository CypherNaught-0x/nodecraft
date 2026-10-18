# 2026-10-17 · Cost admission and usage-log retention (#214)

- **A guess is admitted provisionally.** `cost::check` returns an `Admission`. When a session or
  project cap is set, paid steps quoted at one job before their job count is known are listed as
  `provisional`: the run may start, but the check didn't cover their real cost. The guarantee is
  job-time enforcement in `engine::jobs` (#207, #222), which checks each job as it starts. A known
  empty job list quotes zero and blocks nothing.
- **Charge identity outlives the list.** When an entry leaves the list, a 16-byte digest of its
  provider and key is kept (saved with the log, up to 250,000 of them), so a late retry of it is
  still the same charge. The boundary is explicit: a retry of a charge more than
  `MAX_USAGE_ENTRIES + MAX_RETIRED_KEYS` charges old counts again.
- **Retained bytes are bounded.** Listed entries hold at most 32 MiB of text, and the oldest
  leave first, as at the entry limit. With long fields that is about 16,000 entries.
- **Open charges stay settleable (#228).** An open charge (an estimate or an unresolved one)
  pushed off the list is kept as *pending*, up to 10,000 of them and saved with the log. It can
  still be settled, released or found with `charge`. Once settled, only its identity is kept.
  Past the limit, it stays charged as it is, which is conservative. `settle` enforces the same
  total ceiling as `record`. At most 1,024 sessions keep their own totals. The least recently charged one folds into an
  "earlier sessions" total saved under the reserved empty name. Two kinds of session never fold:
  the one charging, and any with an open charge (#233). Otherwise a reused session id would
  settle an old charge out of its new total and undercount. Sessions kept for open charges can
  exceed the limit, by at most the number of open charges, until the next new session folds
  them; loading allows that. The session
  clock forgets folded names, so it stays as small as the session map. Settling or releasing a
  folded session's entries adjusts the earlier-sessions total, so the project total and the
  current session's stay exact and a saved log always reloads. (Logs saved before #233 can hold an open charge whose
  session already folded; if that session id charges again, the old undercount can recur. Session
  ids are per launch, so this needs an old log and a reused id.) A session id that charges again after
  being folded starts a fresh session total. Entries with an empty session id were never written
  by NodeCraft and are now refused. The CSV export can stream (`write_csv`).
