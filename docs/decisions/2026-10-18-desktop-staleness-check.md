## 2026-10-18 · Explicit desktop staleness diagnostic (Part of #339)

The palette supplies the current canvas only to `canvas.stale`; other command entries retain
their existing empty parameters. Availability and filtering use the command's cheap enabled
check. Only explicit selection dispatches the read-only hash query.

The shell retains one summary of at most 1536 UTF-8 bytes, stamped with retained Session
identity, project, canvas and document revision. It presents the engine's separate stale and
invalid-checked-result counts, first checked node, actual cause and bounded reason without
cloning the report or resolving node labels. Counts can overlap. Unrecorded results are never
certified fresh. The wording describes a last check, including when trusted external kind state
changes without a document revision.

Notice painting follows synchronous drop, palette and Turn into effects, with a final stamp
check before paint. Existing draft notices and foreground palette/menu precedence remain.
No repaint, search, palette filtering or invalidation hashes inputs. Tests use a dev-only format
dependency for actual Open/Save fixtures; the shipping dependency graph is unchanged.
