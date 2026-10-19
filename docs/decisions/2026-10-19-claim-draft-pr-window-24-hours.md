## 2026-10-19 · A claim needs a draft PR within 24 hours

A `/claim` now has to show a draft PR within **24 hours**, down from 72. `CLAIM_HOURS` in
[`../../scripts/claim/claim.js`](../../scripts/claim/claim.js) is the single source; the bot's reply, the
takeover message and the sweep all read it, and [`../../AGENTS.md`](../../AGENTS.md),
[`../../CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`../../README.md`](../../README.md) quote it.

- **Why shorter.** A claim exists only to stop two people building the same thing. Three days of
  silence is three days the next person waits for a signal that never came, and an empty commit is
  always available to stop the clock: `git commit --allow-empty -m "claim: #N"`. Most work here is
  written by an agent that can open the draft inside a day, so the long window protected nobody who
  was actually working.
- **Nothing else moved.** The **14-day push rule** (`PR_IDLE_DAYS`) is unchanged: once a PR exists,
  the claim is held by pushes, not by this window. `/unclaim`, the difficulty-label check and
  "lowest-numbered open PR wins" all behave as before.
- **The sweep sets the real release time.** It runs daily at 05:17 UTC
  ([`../../.github/workflows/claim.yml`](../../.github/workflows/claim.yml)), so a claim becomes releasable
  24 h after `in progress` was added and is freed at the first sweep after that — worst case nearly
  48 h. The bot's reply names the exact expiry, which is the number to trust.
- **Maintainers are exempt, as before.** A claim held by the owner or anyone with write access is
  never released by the sweep and never taken over, so this tightens the rule for outside
  contributors only.
