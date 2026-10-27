## 2026-10-27 · Admitted composer roles in card geometry (#907)

A generated-image count belongs to the composer when the admitted parameter contract marks
it with `generated_image_count` and its control supports count choices. The renderer already
uses this role to replace the ordinary parameter row. Share that ownership predicate with
pure card metrics so the body does not reserve the replaced row a second time. Do not infer
ownership from provider parameter names.

Keep descriptor-only `world_size` and `world_rect` conservative. Their role-aware variants
accept the existing admitted `Roles`; metrics perform no registry or contract lookup. Use
these variants for renderer bounds and section budgets, lazy payload admission, navigation
and formatting, and creation geometry. Session paths reuse their single admitted record.
The registry-only import-position callback resolves the generation contract once because it
has no session admission record. Unadmitted descriptor-only fallbacks retain their previous
conservative bounds, without eagerly resolving contracts for culled nodes.

Composer visibility, the selected count, and canvas zoom do not change ownership. Preserve
conditional-parameter reservations, preview budgets, finite logical sizes, and shared port
anchors. Test the same contract with and without optional role ownership, including a count
parameter with an unrelated name, and compare the renderer against all admitted geometry
consumers. Native GPU captures use a public mock model and never submit a paid run.
