## 2026-10-18 · Lazy native preview input (#147)

- A resident-only lookup cannot represent native indexed or imported assets. Cards capture
  one source capability without UI I/O and let the existing preview worker verify that input.
- Range selection uses existing operation authority, exact identity/length and indexed CRC;
  a cloned range pins its backing after locks are released. Preview verification does not
  globally seal, pack or publish cache proofs. The existing 256 MiB decoder input ceiling
  applies to this one input, while the 16 MiB small-read helper remains unchanged.
- Busy is transient: release the slot, retain no permanent Failed entry, suppress same-frame
  retry, and do not self-wake. Only a later externally driven frame retries. This deliberately
  does not promise idle eventual progress while storage is busy.
- A workerless host only previews resident immutable bytes under existing inline limits.
  Lazy data is unavailable there, rather than absent. The complete source/read/probe/decode
  attempt shares the existing panic containment; cancellation skips queued capabilities.
- This fixes programmatic native-storage Session hosts. The desktop still needs its separate
  native-storage host wiring. The separately reviewed preview resource-policy work is not
  imported by this correction; its admission/ticket/expiry rules must survive a future union.
