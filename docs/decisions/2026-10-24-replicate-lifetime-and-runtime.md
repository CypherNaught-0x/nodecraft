## 2026-10-24 · Bound Replicate lifetime and retain execution-time evidence (#339)

- Pass the host's polling duration through the invocation-local submit context. Replicate
  sends that duration as `Cancel-After` on creation only, rounded upward to whole seconds.
  Reject durations outside the API's 5-second to 24-hour interval before any request rather
  than silently changing the requested limit. Direct adapter submissions use the existing
  one-hour runner default. Other adapters keep their current behavior.
- The server measures lifetime from prediction creation; the local runner measures its
  deadline from the beginning of polling. These are deliberately distinct clocks. Restoring
  a handle does not create a new prediction or reset its lifetime. Local timeout and cancel
  remain best effort and never promise a refund or prove that nothing was billed.
- Keep successful prediction `metrics.predict_time` as optional integer `execution_ms` in
  provider Usage and recorded job settings. Round valid finite, nonnegative seconds to the
  nearest millisecond; reject telemetry beyond 24 hours. Optional malformed telemetry stays
  unknown while otherwise valid paid outputs remain available. Failure/cancel results retain
  the existing error contract, which has no completed-result usage record.
- Execution time is evidence, not a cost or a billable-unit count. Do not invent monetary
  usage from hardware rates. Existing budget and unknown-price admission rules are unchanged.
  Held results and resumed jobs retain it; cross-node cache copies clear it because the copy
  did not run again. Old settings and held records default to unknown when the field is absent.
- Regression tests exercise actual runner submission/resume, header boundaries and unsupported
  durations, malformed metrics, normal recording, held storage, restart recovery and cache
  reuse. They use mock providers and synthetic fixtures, with no paid requests.

Sources checked 2026-10-24: [Replicate prediction deadlines](https://replicate.com/docs/topics/predictions/create-a-prediction)
and [HTTP API](https://replicate.com/docs/reference/http).
