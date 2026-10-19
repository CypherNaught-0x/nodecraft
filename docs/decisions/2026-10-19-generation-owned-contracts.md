## 2026-10-19 · Catalog-owned generation contracts (#465)

Keep node controls and model settings in separate bounded ownership groups instead of flattening
model meaning into a node-kind namespace. Catalog IDs and mode IDs round-trip losslessly with
128-byte lowercase ASCII `. - _ /` grammar, nonempty non-dot slash segments. Other owner
namespaces keep their existing dotted grammar. Every group schema is validated and disjoint;
model contracts must exactly match their catalog schema. No meaning or unit is inferred from
labels. The verified OpenAI `n` setting shares only generated-image-count semantics.

One immutable RunCatalog identity binds production definitions, UI metadata, quote/preparation
and execution. The runtime-only NodeKind definition snapshot does not change wire semantics.
Targeted checks refuse same-schema/different-routing replacement at an unchanged revision.
Node.describe and generated cards use combined resolution; submission and staleness use enabled
effective values. Hidden Reset values are not re-materialized into persisted overrides.

The current image-only kind has invariant `out` image-collection ports. Borrowed incoming-wire
validation is sufficient for this invariant; future changing-output kinds require whole-proposal
endpoint validation before support. Media-input realization and a paid desktop scheduler host
remain separate work. Unknown prices require explicit partial consent and the existing per-run
unpriced-job limit; a missing estimate never becomes free. Raw production selector edits refuse
until the dedicated source-aware model migration transaction is installed.
