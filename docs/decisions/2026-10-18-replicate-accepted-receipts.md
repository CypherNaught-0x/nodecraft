## 2026-10-18 · Replicate accepted receipts and credential-free delivery (#324)

- Only authenticated API calls receive the API key. [Replicate output-file documentation](https://replicate.com/docs/topics/predictions/output-files/)
  describes direct delivery URLs for browsers/third parties and removal of Authorization handling.
  Delivery GETs carry no bearer key; both API and delivery requests retain no-redirect transport.
- A rejected successful create reply may already represent paid work. An independent complete
  JSON scanner retains only a unique root ASCII ID in fixed 128-decoded-byte storage within the
  existing 2 MiB/depth-16 envelope, then requests one bounded best-effort cancel using a locally
  constructed API endpoint. Bounded semantic decoding recognizes escaped ID/status keys and values;
  keys retain a 256-raw-byte spelling cap. Duplicate semantic IDs reject the receipt; duplicate
  statuses cannot prove termination. Full prediction/output admission remains separate.
  Application metadata/output rejection does not prevent this receipt-only cleanup. Malformed,
  truncated or missing bytes cannot be rescued by substring parsing. No output authority or
  whole-submit nonacceptance proof comes from a lifecycle receipt.
- Return the original failure plus a safe ID/disposition diagnostic; it can survive runner failure
  settlement, but explicit forgetting removes it. This is best effort, not durable typed ownership
  or a guarantee that remote execution/billing stopped. IDs containing the configured secret are
  redacted/refused before handle publication or poll/cancel URL construction.
- Ignored echoed input fields have their own 32-field allowance (known pinned schema: 19);
  non-input fields retain 64, with unchanged aggregate values/bytes/depth and pre-submit echo
  proof. This supports a finite local subset, not unrestricted provider schema evolution.
- [Prediction creation](https://replicate.com/docs/topics/predictions/create-a-prediction/)
  recommends 1–2 second polling; use 2 seconds under host clamping/backoff. Its server-side
  Cancel-After deadline has a different origin from local polling RunLimits and is not currently
  propagated through submit. [Runtime seconds](https://replicate.com/docs/reference/http/)
  do not fit token/currency Usage fields; neither money nor a duration schema is invented here.
  The catalog discloses local image/mask caps rather than raising the bounded echo allowance.
