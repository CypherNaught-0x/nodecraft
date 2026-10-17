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
- **Pricing** is `free`, `per_call` (`price`) or `per_unit` (`unit` = `image`, `second`,
  `megapixel`, `thousand_tokens` or `thousand_characters`; `price`; a `count` parameter and/or a
  `default_count` ≥ 1). The count is the parameter's value *after* schema defaults, so the quote
  matches what runs; `count` must be a numeric control (`unsigned`, `integer` or `slider`, not a
  `choice`, so a priced duration is numeric) whose `min` is at least 1 (0 for `thousand_*`), with
  a default or a `default_count`, and it must be the only parameter that scales the cost.
  `thousand_*` prices are per 1000 counted units. Prices are decimal strings with a currency,
  at most six decimals, and quotes round up. Token- and character-priced models without a
  `count` are quoted at run time.
  Check prices against the provider's pricing page when adding or updating a model, and say in
  the PR where they came from.
- **Swapping models** keeps parameters by name when the new model accepts the value, fills the
  rest from its defaults, and names what was removed (not on the new model) and what was reset
  (out of its range) in a notice.
- Files are untrusted input: at most 4 MiB, 4096 models, 32 modes per model and 16 inputs per
  mode; unknown fields are rejected.
