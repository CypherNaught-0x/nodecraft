## 2026-10-19 · The descriptor contract wraps ParamSchema instead of replacing it (#162)

- **Additive, not a rewrite.** The contract (`nodecraft_types::Contract`) holds the existing
  `ParamSchema` and adds rules beside it. These cover units, default policies, conditions on
  visibility and enabling, the hidden-value policy, what a value affects, meaning tags and
  cross-field checks. Every current kind, catalog entry and stored node keeps working
  unchanged. Kinds opt in by overriding `NodeKind::contract`, and the default is derived from
  the descriptor. Replacing `ParamSchema` would have touched every kind, the catalog and the
  file format for no new capability.
- **No new `Value` variants.** Unset, auto and random seed are default *policies*, and a
  resolution reports them as the value's source. A `Null`/`Auto` value would have changed the
  project file format and every adapter's matching.
- **Problems are issues, not errors.** `resolve` reports every invalid, unknown, missing or
  failed-check problem as structured data, so editors and agents can show them all. Only an
  invalid contract or oversized input is an `Err`. `submission()` refuses while any issue is
  open, which keeps hidden or invalid values from ever reaching a provider.
- **Conditions look back.** `visible_when`/`enabled_when` name earlier parameters only (like
  `visible_if`), so evaluation is a single ordered pass and cycles are impossible. Checks may
  name any parameter. Size is bounded per condition and per contract.
- **Names are not meanings.** Migration keeps a value only with the same control kind, the
  same unit, matching meaning tags (when either side has one, and always across namespaces,
  that is on a model swap), and a value valid in the target. Within one namespace the owner's
  own name is its promise, so descriptor upgrades still carry untagged values.
  Everything else is reported with a reason and never coerced. The bulk panel uses the same
  compatibility rule to choose its shared fields.
- **Ports declare roles and wire bounds; modes are optional data.** A kind with declared modes
  gets ambiguity and unsupported-combination errors from `resolve_mode`. A kind without them
  keeps its `NodeKind::mode` predicate, so existing kinds are unaffected.
- **The run path moves later.** Planning still resolves through `ParamSchema::resolve`; it
  moves to `Resolution::submission()` before any kind declares conditions or checks.
- **Out of scope for v1.0:** generation snapshots, dynamic option sources, nested records, and
  UI consumption of the new rules. See `docs/descriptor-contract.md`. The specification is
  [`descriptor-contract.md`](../descriptor-contract.md).
