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
- **What a mode sends, and its audio (#559).** `params = ["seed"]` on a mode lists the only
  model settings that mode sends. A lip-sync mode whose clip follows its speech has no
  `duration`, for example. While that mode is the wired one, the card hides the other settings
  but keeps their values, and run admission accepts the narrower submission exactly. Without
  `params` a mode sends every setting. Every listed name must be one of the model's
  parameters, and a mode with a list must include any parameter that pricing counts.
  `audio = { role = "speech", duration = "follows_audio", max_ms = 30000, longer = "refuse",
  sound = "input" }` describes how a mode treats one of its audio inputs:
  - `duration`: `follows_audio` or `fixed`.
  - `max_ms`: the longest track it takes, 1 ms to 1 hour.
  - `longer`: what happens to a longer track. `refuse` (the default) stops the run before
    anything is sent and asks the user to trim it; `provider_trims` lets the provider cut it.
  - `sound`: the result's soundtrack, `input`, `generated` or `silent`.

  The picker shows this per mode. A track's length is only checked when it's known; unknown
  lengths go to the provider. Imported and generated sounds have theirs read from the headers
  (#608): WAV, FLAC, Ogg Vorbis and Opus, MP3 (Xing/VBRI or constant bitrate), ADTS AAC and
  MP4 audio. A file whose headers don't say, or are damaged, has an unknown length.
- **Source video edits (#179).** `edit = { operation = "object_swap", role = "source",
  min_ms = 4000, uses_ms = 30000, min_pixels = 409600, formats = ["video/mp4"],
  sound = "unknown" }` says a mode edits
  its wired video. `operation` is `motion_transfer`, `object_swap` or `restyle`, the same for
  every provider that does that edit. `role` must be the mode's one required video input, and
  the mode must make a video. `min_ms` is the shortest source it takes, `uses_ms` how much of a
  longer one the provider uses from the start, `min_pixels` the smallest frame (width × height),
  `formats` the file types the source may be (any video when left out), and `sound` is
  `source`, `silent` or `unknown`. A source of another type, or one too short or too small,
  stops the run before anything is sent, when its length or size is known.
- **Trained styles (#167).** A mode whose output is `style` trains one from its wired images;
  a role whose modality is `style` takes one. Such a model names `style_family = "flux-1"`, the
  base-model family a trainer trains for or an inference model takes, and only such models may
  set it. A style trained for one family is refused, before anything is sent, by a model of
  another (`docs/decisions/2026-10-21-trained-styles.md`).
- **Models no card offers (#9).** `cards = false` keeps a model out of model pickers, model
  selection and the Generate cards' model lists. It is still priced, quoted and admitted for
  the one card kind that names it. `comfyui.workflow` is the only such model: the ComfyUI
  workflow card runs the workflow imported onto it, takes its settings from that workflow, and
  so has no parameters and no input roles. Without the key, cards offer the model.
- **Parameters** use the node parameter schema (`ParamControl` in `crates/types`): resolutions,
  aspect ratios and durations are `choice` or numeric controls. Integer values are written as
  strings (`"28"`) so 64-bit values survive JSON.
- **Pricing** is `free`, `by_usage` (`currency`), `per_call` (`price`), `per_source_second` or `per_unit` (`unit` = `image`, `second`,
  `megapixel`, `thousand_tokens`, `thousand_characters` or `thousand_steps`; `price`; a `count`
  parameter and/or a `default_count` ≥ 1). The count is the parameter's value *after* schema
  defaults, so the quote matches what runs; `count` must be a numeric control (`unsigned`,
  `integer` or `slider`, not a `choice`, so a priced duration is numeric) whose `min` is at least
  1 (0 for `thousand_tokens` and `thousand_characters`), with a default or a `default_count`, and
  it must be the only parameter that scales the cost. `thousand_*` prices are per 1000 counted
  units; trainers use `thousand_steps`, counted from their steps setting. Prices are decimal strings with a currency,
  at most six decimals, and quotes round up. Token- and character-priced models without a
  `count` are quoted at run time.
  `per_source_second` (`param`, `prices`) prices a source video edit by each started second of
  the source the provider uses (after `uses_ms`). `param` is a `choice` setting with a default,
  and `prices` maps each of its options, and only those, to a price in one currency:
  `pricing = { type = "per_source_second", param = "resolution", prices = { "480p" = "0.318 USD",
  "720p" = "0.681 USD" } }`. Every mode must declare the same `edit` role and `uses_ms`, and send
  `param`. A source whose length isn't known yet leaves the step unpriced, never free.
  `per_unit_by_choice` (`unit`, `param`, `prices`, and a `count` parameter and/or a
  `default_count` as for `per_unit`) is `per_unit` at the price `prices` lists for the chosen
  option of `param`, with the same rules for `param` as `per_source_second`: a Veo clip is priced
  per second by its resolution (`count = "duration_seconds"`), a Gemini image per image by its
  size (`default_count = 1`).
  `by_usage` describes composite measured usage without a reliable pre-submit amount. It always
  returns an unknown quote, never zero: finite caps refuse it; an uncapped authorized host must
  make the incomplete price explicit. Token counters are not an exact monetary invoice.
  Check prices against the provider's pricing page when adding or updating a model, and say in
  the PR where they came from.
- **Token rates** (`usage`, optional) charge a finished job what it measured: `usage = { input =
  "0.002 USD", output = "0.01 USD" }` per 1000 input and output tokens, with an optional
  `long = { above = 200000, input = "…", output = "…" }` used for the whole request when its
  input tokens exceed `above`. Each part rounds up. The quote still comes from `pricing`, which
  must be paid in the same currency; when the provider reports tokens but no cost, the job is
  charged its tokens at these rates instead of the estimate (a reported cost still wins). A step
  that combines several models has no single set of rates and is charged its estimate.
- **Swapping production generation models** uses the source and destination's model-owned
  parameter contracts. A shared name alone does not establish shared meaning: control types,
  units, meaning tags, target options/ranges and conditional reset rules participate. The
  complete preview names dropped/reset fields and destination defaults before Apply; it never
  includes dropped user values. The node-owned prompt, config and history remain recoverable.
  See [model selection](model-selection.md). The standalone `catalog::migrate` helper remains
  the earlier schema/name migration API; it is not the generation command's semantic authority.
- **Auto-select** (`Catalog::select`, #78) keeps the models that fit what is wired in, whose
  provider the person can use and whose price per job fits the budget. The price is quoted with
  the node's parameters that the model knows, plus its defaults. Known values are checked
  against their controls in place before anything is copied; one that doesn't fit refuses the
  model (`Rejected::Invalid`), and parameters the model doesn't know are ignored. A map of more
  than 256 parameters (more than any schema allows) refuses every model. Survivors are ranked
  by effort:
  - *fast*: quickest, then best quality;
  - *best*: best quality, then quickest;
  - *balanced*: quality minus a waiting penalty of 0 up to 30 s, 1 up to 2 min, and 2 beyond.

  Ties go to free models, then known prices grouped by currency code (alphabetically; amounts
  in different currencies are never compared) and cheaper first within a currency, then
  usage-priced models, then the id. The order is total, so ranking is the same for any input
  order.

  While a budget is set, usage-priced models and models priced in another currency are refused,
  since they can't be shown to fit. Every refused model keeps its reason. When nothing fits,
  `why_none` reports the closest miss. "Use multiple models" (`Selection::several`) takes up to 8
  fitting models, one per provider first, in ranking order. Phase 1 counts inputs only:
  reference, style and voice capabilities, the combined multi-model quote and the canvas command
  come with the picker.
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

## Model-owned parameter contracts and production image kind (#465)

A model may supply `parameter_contract`: its owner must be `model`, namespace must equal its
lossless catalog ID, and its schema must equal `params` exactly. It adds shared visibility,
enabling, reset and cross-field rules. Selection, quote and executable admission use the same
effective submission map; they never reinsert a disabled default. Missing contracts preserve
the legacy schema behavior and imply no cross-model semantic compatibility. Only the verified
OpenAI image `n` controls currently share a generated-image-count meaning/unit. Matching labels
or names do not establish compatibility for other built-in settings.

The standard engine Session registers `generation.image` from one immutable catalog snapshot.
Its node-owned controls are model and prompt; model-owned controls retain their original
namespace. Default-registry composition does not install credentials, a scheduler or a paid
desktop run host. The current production planner supports ordered connected/local text prompts
and image outputs through the existing authorized engine pipeline, plus wired media on the
roles the selected mode declares (#502, read from the asset store at conversion). Unknown
models, undeclared or mismatched roles and unsupported versions refuse explicitly; a `random_seed` default is drawn per run
(see `descriptor-contract.md`). Saved unknown
model IDs remain unchanged and readable. Model replacement requires the separate #157 migration
transaction; ordinary parameter editing cannot replace this production selector.

Generated cards resolve the combined contract and retain separate visible/editable metadata.
A document revision and immutable paired catalog binding certify their bounded cache entries;
same-revision resource replacement invalidates them. The constructor has a checked64 MiB logical
metadata ledger (including mode-by-union-port rows), and the existing card cache remains16 MiB.
These are logical retained-owner scopes, excluding caller-owned catalog, spare capacity,
allocator/RSS and arbitrary native callback peaks. Existing execution/output limits remain separate.
