## 2026-10-23 · Known media bounds precede upstream paid work (#795)

Generation cards check the media metadata already present when the run is planned, even when
an upstream card has yet to produce their prompt. A PDF over the page limit, source video the
edit refuses, or audio the mode cannot trim refuses the run before any provider request. The
complete plan and this preflight share their validators and actionable messages.

The pure `NodeKind::check_known_inputs` hook receives borrowed, already-budgeted input facts;
it creates no job, endpoint selection, cost witness, filesystem read or provider request. The
default does nothing for existing custom kinds. Native callbacks keep the same private error
provenance as their complete planning hooks. The normal complete plan still runs after inputs
arrive and remains authoritative for role counts, content, routes and provider jobs.

An unresolved scalar contributes one input value; an unresolved list may contribute zero or
more. Generation considers every declared mode whose cardinality can still fit those facts.
It refuses only when every feasible mode produces the same known-media error. Otherwise it
waits for the real input, so a missing list cannot accidentally select a stricter mode or be
treated as empty. If no completion can fit any mode, the normal contract or one-mode role-count
refusal is already proven and is returned now. Unknown duration, dimensions and page counts retain their existing policy.

Every bounded fanout cell receives the check, including cells after an earlier deferred one.
The hook borrows both text and media metadata without cloning them, and never reads media bytes. It
therefore adds no decoding or background work to admission. This closes the deferred-prompt
gap recorded in [the PDF-input decision](2026-10-23-pdf-document-input.md).
