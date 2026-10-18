## 2026-10-17 · Reservation ownership across re-registration (#263)

- A provider/key replacement closes the previous reservation even when the replacement has no
  JobPrice or is cancelled before submission. A previous in-flight submit may already be billed
  before a handle arrives, so its estimate becomes unresolved. Unpriced work still adds no charge.
- Submitted reports, completion/failure and dropped-future accounting affect only their captured
  provider/key. Running ownership also follows that identity, so an old guard cannot consume a
  replacement's deferred settle/forget or release its pending charge. Matching retries retain
  their reservation and ordinary cap enforcement. This does not redesign provider-handle lifecycle
  or authenticate overlapping invocations with the same provider/key.

