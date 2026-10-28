## 2026-10-27 · Cancel superseded live actions and meter each round (#596)

Each live job owns the engine's adopted-document lineage token, a cancellation signal and
one cumulative quota of 100,000,000 work
units. The host signals the old owner before polling its worker when the session identity,
canvas, document revision or active preview draft changes. Keep its worker until it exits;
starting a replacement never creates overlapping live workers. Equal project ids and
revision numbers do not authorize a replaced session's output or a document opened/recovered
in the same session. Completed and cache-only
outcomes retain the signal, so cancellation before publication also refuses their results.

Use deterministic units rather than a wall clock: the same counter works on single-threaded
WebAssembly. Charge 1,024 units per action plus parameter/input value and text-byte costs;
charge encoded media bytes before copying or reading them. CPU image kernels, masks and
layer compositing admit their existing reference work estimates once. PDF interpretation
and plugin cancellation checkpoints spend 64 units; drawing a PDF page also admits its
bounding-square pixels. Encoding a staged image spends one unit per pixel. The quota is
shared across nodes, actions, pages and callbacks, never replenished within a round.

Pure action/media APIs accept optional synchronous callbacks. Nonzero calls admit work before
proportional execution; zero calls only poll cancellation. CPU kernels check between bounded
pixel chunks and manual blur rows, masks between rows/chunks, and composites between image,
layer and graphics passes. PDF callbacks reach operators, glyphs, text-array items and nested
content through the upstream checkpoint hook. These callbacks return errors rather than
successful partial images or text. The PDF page-text limit remains a separate intentional
truncation and does not cancel its owner or later pages.

Cancellation discards the whole obsolete outcome. Quota exhaustion is an actionable node
failure, retained with its input hash so idle frames do not repeatedly run the same expensive
input. Narrowing a PDF range or changing another input allows a fresh attempt. Valid completed
nodes still use the normal derived-result transaction. Explicit runs keep their existing
operation limits; the message explains that alternative. No paid provider is started live.

This is cooperative cancellation and a work quota, not a wall-clock deadline. Individual codec,
PDF-open and graphics operations already in progress remain boundaries, covered by their
existing input, allocation, work and driver-timeout limits. Default library callers retain
those existing limits when they do not supply a callback. No timing policy enters shared
PDF code, and no new command or document mutation path is introduced.
