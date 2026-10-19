## 2026-10-18 · Native storage capability is separate from archive wire version (#348)

A native sealed ArchivePlan selects numeric version 1 only when its exact prepared stored-ZIP
layout fits every frozen small-host limit and uses no ZIP64 size, offset, extra or footer.
Otherwise it selects numeric version 2, retaining the existing large limits and frozen graph
schema gate. Shared manifest and asset-count limits remain hard errors for both versions.

Both choices retain StreamingV2 capability, native leases, private immutable bytes and typed
publication authority. Byte-only host defaults still refuse native plans before I/O. Native
NCRJ0002 checkpoints keep their 80-byte identity-bound envelope with either embedded archive;
Save and checkpoint consume the same ArchivePlan. Small hosts still emit NCRJ0001.

This preserves ordinary small native saves for existing v1/browser readers without buffering
native content. Existing numeric-v2 files remain readable. It does not assign Comments graph
fields to numeric v2: the separate #194/#359 format-family coordination must distinguish that
historical schema explicitly before integration. Historical fixtures retain their actual bytes.

Operation try_lock reports Busy rather than waiting. Synchronous storage work and multiple
full disk owners remain explicit limitations; off-UI I/O, progress/cancellation and recovery
copy reduction are separate follow-up work, not consequences of selecting wire v1.
