## 2026-10-23 · Split rollback releases provisional ownership, not hash keys (#806)

- **A hash is content identity, not ownership.** A failing Split Video could remove a part
  another concurrent action had successfully produced after the split's key snapshot. Opaque,
  non-Clone receipts now hold each unpublished part explicitly. Failure, cancellation or unwind
  drops those holds; only the last hold on an unpublished entry removes its bytes. A successful
  ordinary action or another successful split marks the shared entry published, so rollback
  cannot remove it. This supersedes the key-removal rollback described in
  [render staging](2026-10-23-render-staging.md).
- **Publish the whole split under one lock.** Every part is rendered through the existing
  native composition/read-back adapters, with an explicit provisional result. Once every part
  succeeds and the final cancellation check passes, publication validates all receipt owners
  before making the batch visible. Host import and downstream reads expose only published
  entries. Ordinary video and extracted-sound results retain their immediate publication.
- **Retain the run's limits and chaining.** Unique encoded bytes plus concurrent read buffers
  share the existing 1-GiB budget. Unique entries plus pending reads still have a 4,096-result
  ceiling; provisional receipts plus pending reads independently have that same ceiling to bound
  ownership metadata, including duplicate parts. Duplicate content retains one payload and one
  unique-result charge. Admission remains conservative before a hash is known. The split's
  existing 32-part and 1-GiB logical-part limits remain separate. Published bytes stay available
  until the run executor drops, because its original project snapshot predates those outputs.
- **Exercise the production split loop.** A bounded renderer seam allows ordinary tests to
  stage a part, accept an identical sibling result on another thread, then fail or cancel a
  later part. The actual host snapshot and downstream byte reader must still resolve the
  accepted result. Other tests cover no-consumer cleanup, overlapping provisional splits,
  complete publication, final cancellation and refunded byte/result/receipt admission. An opt-in
  native fixture checks real split durations, audio and extracted sound, with sole-owner service
  cleanup even when its assertions fail. Shared test services retain their existing lifetime.
