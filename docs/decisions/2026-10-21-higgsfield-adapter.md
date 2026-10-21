## 2026-10-21 · Higgsfield replays the exact staged body and prices by usage (#177)

- **One secret, two parts.** Higgsfield authenticates with a key id and a key secret. The
  keychain keeps one secret per account, so the `higgsfield` account stores `<id>:<secret>`; the
  adapter splits it per request and refuses any other shape before sending, and never echoes
  either part.
- **An ambiguous submit is replayed, because the body is too.** Higgsfield honours an
  `Idempotency-Key` only for an identical body, and Kling's body names a freshly staged upload
  link. The adapter keeps the staged body per idempotency key and job fingerprint (bounded,
  in memory), so an in-process retry sends the same bytes and gets the original request back;
  `retry_ambiguous_submit` is true. After a restart the link differs, Higgsfield answers 422
  "used with different parameters", and that is reported as possibly billed and never retried:
  a mismatch never silently becomes a second chargeable request. Persisting the staged body with
  the run would close that gap and is left for later.
- **No catalog price.** Credits cost a plan-dependent amount, so both models are `by_usage`
  rather than carrying one plan's rate. `Higgsfield::estimate` quotes the exact body; wiring
  live provider estimates into run quotes and budgets is a separate engine change.
- **Only documented refusals release the reservation.** 400 (including concurrency), 401, 403,
  404, 422 (but not the reused-key 422), 423 and 503 with a readable reason prove nothing was
  accepted; everything else stays possibly billed.
- **Two models first.** SOUL 2 (text to image) and Kling 2.5 Turbo Pro (image to video) cover
  both output kinds and the upload path. Genjutsu needs a model that takes a video input, which
  the catalog doesn't have yet.
