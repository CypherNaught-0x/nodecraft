## 2026-11-04 · A plugin template's billing claims are shown, not trusted (#174)

**Problem.** A provider template says things of its own service that the host can't check: what
each model costs, and whether the service deduplicates a resent job by its key. Naming an
idempotency header turned on automatic resends of an ambiguous submit, up to three attempts.
Combined with a 1 s `timeout_ms`, a template could make every submit time out after the service
took it and then have it sent again. A service that ignores the key bills each send, while the
budget counted the near-zero declared price once.

**Decision.**

- **No automatic resend, ever.** `Broker::retry_ambiguous_submit` is `false` whatever the template
  says. The host still sends the job's key in the named header, but the job is sent once. A
  submit whose answer is lost ends as possibly charged, and that charge is settled as
  unresolved, as for any paid adapter that refuses resends. The person decides what to do with
  it. We picked this over "one retry, each attempt recorded" because it is the only option where
  no publisher claim can cost money that the host hasn't recorded.
- **Submits get at least 30 s.** `plan::MIN_SUBMIT_TIMEOUT_MS`. A template may lower the timeout
  of polls, cancels and downloads, but never the timeout of the paid request. That keeps
  "cut off while the service was still accepting it" the exception.
- **Claims shown at consent.** `plugins.rights` and the import preview list each provider's
  declared price per job and its idempotency header as `claims`. The grant dialog shows them
  under "Its publisher says (NodeCraft can't check this)", apart from what is granted. Claims are
  never part of a grant's rights.

This supersedes "a lost reply is resent automatically only under the template's idempotency
header" in [the broker adapter record](2026-10-23-plugin-provider-broker-adapter.md).
