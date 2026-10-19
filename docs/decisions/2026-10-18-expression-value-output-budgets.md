## 2026-10-18 · Expression admission and returned-output budgets (#401)

- Validate every input/list/parameter before copying it, with a metered traversal whose auxiliary storage follows depth. Borrow base bindings; meter each inspected name candidate and each produced result, including scalars and forwarding branches.
- Keep schema 1 and the existing lowered limits. A compiled action retains outputs up to four times its value-byte limit separately from the evaluator's four-times tracked allowance. Refusal returns no partial output set; one fuel/cancellation meter covers admission and all outputs.
- These are logical payload charges, not allocator or process-memory limits. The [language reference](../text-expressions.md) states the distinct scope, application, candidate, output and metadata owners. Known-size construction is checked before growth; Unicode conversion preserves Rust's contextual output.
