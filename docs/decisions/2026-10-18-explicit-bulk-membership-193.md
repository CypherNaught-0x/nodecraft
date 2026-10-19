## 2026-10-18 · Explicit bulk membership and palette identifiers (#193)

Bulk tag/field assignments require the membership key. Explicit null clears membership;
omission and unknown assignment fields are errors, so malformed replay cannot silently untag
or ungroup nodes. Both assignment forms use the same bounded ID admission helper.

Tag and Field colours are optional, nonempty palette identifiers: at most 64 ASCII lowercase
letters, digits or hyphens. Unknown identifiers in that vocabulary remain round-trippable.
Tag validation is shared by changeset admission and whole-canvas validation.

The established Tag/Field extension-member behavior is retained for legacy compatibility;
new semantic persisted fields still require a format version bump. Versions 2/3 belong to
Comments and the persisted Tag/Field bridge; streaming uses the separate v4/NCRJ0004 family.
Literal assertions pin the actual v2 fixture's migrated rectangle and collapsed state.

Current v3 writes use the admitted v3 graph model and replay budgets. The frozen v2 role
validator applies only to incoming v2 archives/records; applying it to current output would
reject every migrated Field rectangle and prevent saving or checkpointing the upgrade.
