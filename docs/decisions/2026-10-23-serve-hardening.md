## 2026-10-23 · Serve mode's overall limits (#635)

`nodecraft-cli serve` now holds every run to limits that span runs, not just each run:

- **One spend for the server's lifetime.** The worker gives each run's `Runner` the same usage
  log and budgets (`run::Spend`), and `--max-total` sets the project budget, so the engine's own
  `run.start` budget check refuses a run that would pass it. A project budget is in one
  currency and needs a known price, so with it unpriced runs are refused even under
  `--allow-unpriced`. The count starts again on restart;
  it is a guard against a runaway client, not an accounting ledger.
- **Keys outlive runs.** A key→(fingerprint, run) table with a 24-hour time-to-live, capped at
  65,536 keys, sits beside the 256-run book. A key whose run was forgotten answers `410` rather
  than starting the run again: a retry must never charge twice, and a client that waits a day
  to retry can pick a new key. When the table is full of unexpired keys, keyed requests answer
  `429` instead of evicting a live key. A key whose run is still in the book never expires.
- **Queued uploads are budgeted in bytes** (256 MiB), as well as in jobs (32).
- **A run has a deadline** (`--run-timeout`, default one hour). Past it the host's jobs are
  cancelled and given 30 seconds to stop; the run then fails as `timeout`, saying jobs a
  provider had accepted may still be charged. One hung job no longer stalls the queue.
- **A response has a deadline**: 30 seconds plus one second per 256 KiB, on top of the
  per-write timeout, so a trickling reader can't hold a connection slot for a whole download.
- **The token file is removed** on Ctrl-C or `SIGTERM`, through `TokenFile::remove_if_ours`,
  then the process exits with 130 (Ctrl-C) or 143 (`SIGTERM`). If the signal can't be registered the file is left, as after
  a crash.
