# 2026-10-17 · Provider identity and unreserved costs in budgeted jobs (#228)

- **Handles belong to the provider that issued them.** The runner keys stored handles by
  provider and idempotency key, and `restore_handle` takes the provider. A job moved to another
  provider under the same key never resumes the first one's handle. Its fresh submission is held
  to the caps, and the old attempt's reservation is closed first, as `unresolved` if it may have
  been billed.
- **A reported cost counts even with nothing reserved.** A priced job with no estimate, running
  with no caps, reserves nothing. When it finishes, fresh or resumed, the cost the provider
  reported is recorded as `actual`, once per provider and key. A report the log refuses as
  implausibly large is left out rather than failing the finished job.
