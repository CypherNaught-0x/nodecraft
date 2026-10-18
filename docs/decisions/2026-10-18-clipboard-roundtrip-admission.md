## 2026-10-18 · Clipboard roundtrip admission (#273)

- Cut uses one synchronous Session path: actual Dispatch callbacks describe delete and a
  hypothetical post-delete paste; only Edit effects proceed. Private staging and normal source,
  history and recovery checks precede committing the original delete. No service effect runs,
  and the clipboard text is queued only after commit. Admission covers that captured proposal;
  a subsequent Paste remaps IDs and placement against current state, so arbitrary extensions
  can reject a different future proposal. Trusted native callbacks are not sandboxed.
- Clipboard projection borrows every retained field and excludes generation history at source.
  Logical command and encoded envelope limits are distinct; bounded copies follow admission.
  Generic Session graph/history staging still clones its ordinary bounded representations.
- Destination immutable bytes, not project UUIDs, establish reference availability. Paste and
  UI preflight share format's supported MIME/image-dimension checks, preserving unknown-signature
  compatibility and advisory unsupported metadata. No asset transfer is implied.
