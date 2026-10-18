# 2026-10-17 · Submission certainty (#227)

- **The provider task decides whether a failed submit may have been billed**
  (`providers::RunFailure::uncertain`), because only it knows whether the submit was sent. Any
  of these is uncertain: a submit interrupted mid-flight (cancelled while submitting, timed out,
  cut off, a server error), or a reply that can't be read or says the job ran (`Malformed`,
  `JobFailed`, `Expired`). Job-time budgets (#222) keep those reservations and charge them as
  `unresolved`. A run cancelled before its submit, and refusals (`Auth`, `UnsupportedInput`,
  `BlockedContent`, `Budget`, `RateLimited`, `Busy`), are certain, so their reservations are
  released.
