## 2026-11-04 · Plugin downloads have a lower cap and a per-package byte budget (#174)

**Problem.** The broker buffered each output in memory. One output could hold up to 256 MiB and
one job up to 512 MiB, and the broker then copied the buffer into the shared output. Several jobs
running at once multiplied that. A hostile service could push the host into gigabytes of
allocation and an abort, and nothing let a plugin package's jobs share one ceiling.

**Decision.**

- **64 MiB per job.** One plugin job's downloads hold at most 64 MiB together, and so does any
  single output. That covers images and short clips. Built-in adapters keep their own limits.
- **128 MiB per package.** All the providers of one package share a `ByteBudget`, room for two
  jobs. A job takes its whole 64 MiB before its first download, all or nothing, and gives it back
  when its outputs leave the broker. Jobs never hold part of what they need while they wait, so
  they can't deadlock. Room is always taken before a request slot, never while holding one. A job
  whose wait is dropped holds nothing.
- **Free the reply buffer early.** `Output::Bytes` holds an `Arc<[u8]>`, so making it still
  copies once; avoiding that copy would mean changing the HTTP and output types. The reply buffer
  is moved in and freed as soon as the copy exists, so one output is held twice only during the
  copy.

This supersedes the per-output and per-job sizes in
[the broker adapter record](2026-10-23-plugin-provider-broker-adapter.md).
