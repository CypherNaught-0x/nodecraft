## 2026-10-17 · Cheap read-only command previews (#134)

- **Availability still admits prospective edits.** `Session::enabled` runs handlers in Preview
  and applies frozen-format and history admission to document effects. Read-only queries validate
  their parameters and capabilities first, then may return a null non-document preview effect.
  `canvas.stale` uses this phase contract so only dispatch traverses and hashes the canvas; its
  node registry is the authoritative Session/Context registry. Service and file handlers describe requests; availability
  does not execute their I/O.

