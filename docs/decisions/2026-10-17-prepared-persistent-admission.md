## 2026-10-17 · Prepared persistent admission (#134)

- Session privately pairs a graph-staged draft with a non-Clone forward replay capability and
  source identity/revision. Every document publication, successful import and save/asset GC
  replaces identity; rejected preparation/import/append leaves it unchanged. Commit consumes
  the bundle and checks identity/revision before append. Handlers cannot fabricate this proof.
- A valid v1 root plus complete frozen forward/inverse role admission replaces redundant whole
  prospective-project checks. Changed surviving nodes still validate reference existence, MIME
  and dimensions against the complete source store. Payload reference availability, canonical
  future redo budgets and ordered valid-final batches remain checked. Config is still refused
  by frozen v1. Archive/Save/Open and arbitrary public frame writers retain full admission.
- Owned replay admission captures only referenced immutable asset bytes and derives additions
  from admitted forward references. Append cannot substitute a different document/assets pair.
  Builtin Preview graph staging now uses the same single Session boundary as custom effects;
  read-only Preview still avoids preparing a document. Deterministic opt-in counters establish
  eliminated visits; shared-machine wall timings describe samples rather than guarantee speed.

