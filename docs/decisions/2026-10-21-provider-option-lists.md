## 2026-10-21 · Option lists come from more than one provider, with labels (#179)

- **Higgsfield's restyle presets are an option source.** `higgsfield.restyle_presets` is the list
  Higgsfield's API returns for Genjutsu restyle (`GET /models/higgsfield/genjutsu/restyle/v1.0/presets`).
  The source id is fixed in `crates/engine/src/option_fetch.rs` like the ComfyUI ones, so a
  catalog row still can't point a source at an arbitrary endpoint.
- **Each provider is asked on its own.** A fetch asks ComfyUI only for ComfyUI lists and
  Higgsfield only for its presets. A provider that isn't set up, or doesn't answer, refuses its own
  lists with its reason; the fetch as a whole fails only when no provider could be asked, so a
  missing ComfyUI server never keeps the presets from loading. Both are built through
  `provider_setup` like their run adapters, reading the same key and settings.
- **Values and labels are separate.** A preset's value is its id, which the request sends and the
  card stores, and its label is the name a person reads. ComfyUI lists keep each value as its own
  label. Presets with an id that isn't a UUID, or a blank, oversized or key-echoing name, are
  dropped and counted like a blank ComfyUI value.
- **The latest list wins.** Higgsfield can add or retire presets. Each successful fetch replaces
  the list, and a failed one keeps the presets installed before, like any list. A stored
  `preset_id` that the latest list no longer has is reported on the card like a missing checkpoint.
