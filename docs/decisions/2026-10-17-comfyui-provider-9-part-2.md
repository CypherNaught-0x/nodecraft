# 2026-10-17 · ComfyUI provider (#9, part 2)

- **Prompt ids are salted per install** (`prompt_id_for(salt, key, attempt)`): the same job always
  maps to the same id on one install, and never to another install's on a shared server. The
  salt is random and stored once in settings.
- **A submit checks the server first** (queue, then history): a queued, running or successful
  prompt is reused, so a retried submit never queues twice; a failed one moves to a new attempt
  id. Servers that ignore client-chosen ids lose this check.
- **The queue is read before the history:** a prompt leaves the queue only once it is in history,
  so a run finishing between the reads isn't reported as lost.
- **Workflow errors aren't retried automatically** (`ErrorKind::JobFailed`): running a failing
  workflow again would usually fail the same way and may cost; out-of-memory stays retryable.
- **Checks before uploads:** value overrides and `check()` against `/object_info` run before any
  input is uploaded; uploads are named by content hash, so overwriting is harmless.
- **Only `output` files are results;** a preview-only workflow fails with a clear message. Unknown
  file types are skipped; a text page served for a binary file is refused (proxy login pages).
