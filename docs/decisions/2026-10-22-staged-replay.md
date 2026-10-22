## 2026-10-22 · A paid request is saved before it is sent, and replayed after a restart (#664)

- **The adapter stages the exact bytes.** Higgsfield already kept the body it sent per key, in
  memory, so a lost reply could be retried without new uploads. It now also hands that body to
  `SubmitContext::stage` before `paid()`, as a `StagedSubmit { body, data }`. The engine saves it
  in the run journal, with the job and an empty handle, synchronously. A request that can't be
  saved isn't sent: sending without a durable copy would bring back the double-charge risk this
  closes.
- **A restart replays and never rebuilds.** A journal entry with no handle but a staged request is
  resumed through `Provider::replay`, which sends the saved body under the same Idempotency-Key.
  Building the request again would upload again, its links would differ, and Higgsfield would
  refuse the key as used with different parameters. A replay that fails for good is reported as
  possibly billed with a note that the links may have expired; it is never uploaded and sent anew.
- **Opt in per adapter.** `Provider::replay` is "unsupported" by default, and only an adapter that
  calls `stage` produces a staged entry. Other adapters keep the earlier rule: a job with no handle
  after a restart is settled as unresolved.
- **The journal format stays version 1.** `Submitted.staged` is optional and left out when empty,
  so a journal written before this reads unchanged. A journal written by this version is refused by
  an older NodeCraft (deny unknown fields) only while a request is staged, which lasts from the
  send until its reply.
- **Bounded.** A staged body is at most 1 MiB; the adapter checks that a staged body is a JSON
  object for the model it names before it sends anything.
- **A replayed job counts as possibly accepted.** Its reservation starts possibly billed, like a
  job followed by its handle, so a refused replay leaves the charge unresolved rather than
  releasing it: the lost POST may have been accepted.
- **The journal is trusted to define the request.** It is a local file NodeCraft wrote; the replay
  checks only that the body is a bounded JSON object and that the saved handle record (model,
  output count) is one polling accepts, so a damaged entry is refused before anything is paid.
