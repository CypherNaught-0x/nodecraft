## 2026-10-23 · Bound rendered media retained by a run (#584)

Local actions keep rendered videos and extracted sound available for later actions in the same
run. The session's asset snapshot predates those results, so importing a completed result into
the session does not make its staging copy dispensable. We retain that chain behavior and cap
the encoded staging payload at 1 GiB per run, matching the existing ceiling for one video split.
At most 4,096 retained results and unfinished reads bound the index metadata as well.

Every read-back reserves its declared length and a result slot before allocating or requesting
a chunk. Retained payload and concurrent reservations share one mutex-protected budget. A
reservation refunds itself after errors or cancellation; successful content-addressed insertion
keeps one copy per hash. Split rollback removes its new parts through the same accounting.
Admission is conservative: a read that might duplicate an existing result still needs temporary
capacity until its hash is known. Full runs fail with guidance to render fewer nodes or use a
lower quality, rather than reading another whole output before discovering the limit.

The existing 256 MiB single-result limit remains. These limits cover retained encoded payload
and admitted read buffers, not total process RSS: chunk storage, Vec-to-Arc conversion, decoder
frames, the project asset store and the video service's disk quotas are separate bounded phases.
No new filesystem path or provider call is introduced. Completed jobs retain the run host's
existing per-step recording semantics; a failed or cancelled read publishes no partial result.

Small-limit tests exercise the exact production reader with concurrent reservations, failures,
last-chunk cancellation, duplicate hashes and rollback. Mock-provider fixtures test accumulated
outputs without network calls. An opt-in test additionally encodes several tiny Timelines with
mock-generated captions, lowers only the test budget and checks that the next render is refused
and the test-owned service cleans up. This does not imply actual tool execution on every platform.
