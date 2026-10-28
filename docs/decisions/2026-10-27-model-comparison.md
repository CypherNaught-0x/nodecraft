## 2026-10-27 · Explain model selection tiers and descriptions (#963)

The catalog's `quality` integer participates in automatic selection and picker ordering. No
shared benchmark or calibration across modalities was recorded for it. Present it as
**Selection tier**, an editorial priority from 1 (lowest) to 5 (highest), rather than an apparent
measured quality score. Retain the serialized field, integers, ordering and selection algorithms:
changing these would silently change existing workflows and requires separate evidence.

Show each model's concise description directly in the virtualized picker, with a full hover
version when the bounded line truncates. Keep the configured time hint and price on separate
lines. Time is metadata, not a measured latency guarantee. Model purpose and relative strengths
come from primary provider documentation; [the audit](../model-comparison.md) records a checked
date and source for each shipped id. Descriptions cover only the modes NodeCraft exposes.

No schema migration is needed: descriptors already carry descriptions and the existing numeric
fields. New models must add a source row to the audit; a catalog test enforces complete coverage.
NodeCraft does not infer numeric tiers from vendor marketing or compare unrelated tasks as if
one five-point benchmark existed. A future benchmark must state its task, method and date before
claiming measured quality.
