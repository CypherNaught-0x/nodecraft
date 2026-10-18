# Model catalog

Models are data. Each file in `assets/catalog/` lists models for one provider and is embedded
in the build (`nodecraft_catalog::Catalog::builtin`). Adding a model on an existing provider is
a data change; run `cargo xtask catalog-check` (also part of `cargo xtask ci`) before committing.

```toml
[[model]]
id = "fal.flux-dev"              # NodeCraft's id: lowercase letters, digits, . - _ /
provider = "fal"                 # the adapter that runs it
provider_model = "fal-ai/flux/dev"  # the provider's own model id (or ComfyUI workflow template)
label = "FLUX.1 [dev]"
description = "One line for the picker."
latency_s = 8                    # typical seconds to a result
quality = 4                      # 1 (draft) to 5 (best)
# Illustrative price only: always take real prices from the provider's pricing page.
pricing = { type = "per_unit", unit = "image", price = "0.01 USD", count = "num_images" }

[[model.modes]]                  # one per way of using the model
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model.params.params]]          # the same parameter schema node kinds use
name = "num_images"
label = "Images"
control = { type = "unsigned", min = "1", max = "4" }
default = { type = "unsigned", value = "1" }
```

- **Modes and input roles.** A mode names its output modality and the inputs it takes, each
  with a role (`prompt`, `image`, `first_frame`, …), a modality and how many (`min`, `max`;
  defaults 0 and 1). A text role marked `inline = true` is filled by the node's own prompt
  field when nothing is wired (a wired text input replaces it). The picker enables a model when
  one of its modes takes what is wired into the node; otherwise it shows why (e.g. "Takes at
  most 1 image input"), naming the closest mode's mildest problem.
- **Parameters** use the node parameter schema (`ParamControl` in `crates/types`): resolutions,
  aspect ratios and durations are `choice` or numeric controls. Integer values are written as
  strings (`"28"`) so 64-bit values survive JSON.
- **Pricing** is `free`, `by_usage` (`currency`), `per_call` (`price`) or `per_unit` (`unit` = `image`, `second`,
  `megapixel`, `thousand_tokens` or `thousand_characters`; `price`; a `count` parameter and/or a
  `default_count` ≥ 1). The count is the parameter's value *after* schema defaults, so the quote
  matches what runs; `count` must be a numeric control (`unsigned`, `integer` or `slider`, not a
  `choice`, so a priced duration is numeric) whose `min` is at least 1 (0 for `thousand_*`), with
  a default or a `default_count`, and it must be the only parameter that scales the cost.
  `thousand_*` prices are per 1000 counted units. Prices are decimal strings with a currency,
  at most six decimals, and quotes round up. Token- and character-priced models without a
  `count` are quoted at run time.
  `by_usage` describes composite measured usage without a reliable pre-submit amount. It always
  returns an unknown quote, never zero: finite caps refuse it; an uncapped authorized host must
  make the incomplete price explicit. Token counters are not an exact monetary invoice.
  Check prices against the provider's pricing page when adding or updating a model, and say in
  the PR where they came from.
- **Swapping models** keeps parameters by name when the new model accepts the value, fills the
  rest from its defaults, and names what was removed (not on the new model) and what was reset
  (out of its range) in a notice.
- Files are untrusted input: at most 4 MiB, 4096 models, 32 modes per model and 16 inputs per
  mode; unknown fields are rejected.

## Catalog-bound execution admission (Part of #275)

`exec::admission::admit_run` owns the exact plan/canvas, selected catalog schema witness and
canonical job parameters. Generate jobs must match a selected catalog provider and remote
`provider_model`, with effective visible parameters equal to the frozen node's values. Defaults
are sent explicitly; hidden values are removed, while unknown keys and invalid hidden overrides
are refused. Required visible values use executable readiness even for free models. Aliases with
conflicting successful canonical interpretations are refused. Every Model control participates
in selection/pricing, including defaulted and hidden controls; multiple selections keep their
conservative summed estimate. Known usage-based pricing can remain partial, but partial consent
cannot bypass invalid generation identity/schema. Empty work remains empty.

The complete job vector is admitted before JobIds or any scheduled prefix, including deferred
work and Source-category callbacks emitting Generate. Legacy `Run::start` supports local/source
work only; generation uses the consuming `Run::start_admitted`. The general converter and raw
provider APIs remain trusted low-level host interfaces. Native kind callbacks must remain pure
and stable; this API is not a sandbox for arbitrary Rust side effects.

Shared work limits are64MiB per step/256MiB per run for retained inputs plus canonical jobs,
including inserted defaults and repeated owned occurrences. Charges transfer into Run, tighten
with lower start limits and remain sticky across deferred refusal. Witness/schema/typed request
metadata has a separate64MiB cap; live admission scratch16MiB and a single preflighted Canvas
snapshot256MiB. Logical measurement uses Rust shells, collection elements,48 bytes of map/set
linkage per entry and owned payload lengths. It bounds logical preparation ownership, not spare
capacity, allocator overhead, process RSS, native callback peaks or arbitrary host queue totals.
Existing InputHasher, scheduler/dependency indices and later output/history retention keep their
independent policies;592MiB is a per-preparation envelope, not a whole-Run memory claim.

Engine hosts register `Session::register_run_host`, enqueue the opaque non-Clone PreparedRun,
then consume it through `Session::authorize_prepared_run` outside dispatch. This rechecks the
actual live Session/source/catalog/request binding, accepted totals and current budgets without
re-planning. Stale preparations refuse; they do not silently reprepare or reserve money. Ordinary
JSON registration of `run.start` is refused. Preview availability does not clone a canvas or
construct executable preparation. No desktop/CLI scheduler host is supplied by this change.

The retained RunPlan and Usage root shells are charged exactly once to the total-run work
ledger before selection/report/schema copies, including an empty plan. They are not a per-step
fee; lower start limits retain this charge. Quote model lists contain validated catalog IDs only.
Unknown raw selections are excluded independently of any later missing/usage-priced control's
aggregate reason; valid selected IDs and the all-control pricing policy remain intact.
