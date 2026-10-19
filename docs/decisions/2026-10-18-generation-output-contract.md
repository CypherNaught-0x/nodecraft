## 2026-10-18 · Generated output routes match stored instance ports (Part of #275)

Before admitting a step, exec validates every Generate job route against the actual node's stored output ports: the job must belong to that step’s node, the name must exist and its modality must match. Per-instance ports are authoritative; palette descriptors are not substituted. Several routes may share a modality, including duplicate valid routes and list ports. Source jobs retain their existing behavior.

The eager planner reports an invalid-node blocker when inputs are known. Run repeats the same private validation on the complete job vector after deferred materialisation or a public RunPlan, before allocating any job ID or exposing scheduler work. A rejected vector publishes no prefix. Diagnostics are static so malformed programmatic route strings cannot leak into errors.

This addresses requirement 6 of #275 only. Catalog/quote binding, instance lanes, mock placement, Driver golden coverage and scripted retry behavior remain separate follow-ups.
