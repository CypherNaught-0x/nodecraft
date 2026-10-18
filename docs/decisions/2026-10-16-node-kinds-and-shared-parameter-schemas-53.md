# 2026-10-16 · Node kinds and shared parameter schemas (#53)

- **Schema data lives in `types`, re-exported by `nodes`.** Both node kinds and catalog models
  need the same controls; putting the pure data below both L2 crates avoids a lateral dependency.
  Controls cover text, choices, toggles, sliders, integers, colours, model IDs, seeds and assets.
  Schema/default/condition validation runs before any parameter cloning. Conditions reference
  earlier parameters; hidden controllers also hide their dependants, so there are no cycles.
- **Parameters use real scalar values.** `Value` now includes booleans, finite `Number`s, signed
  integers and unsigned integers. Integer values and integer bounds use decimal strings on the
  wire so full-width seeds/counts survive browser JSON. Number construction rejects NaN/infinity;
  workspace `serde_json` enables `float_roundtrip` so defaults and equality conditions survive
  save/load exactly. These variants are part of the initial format; later additions remain
  subject to the format-version gate recorded below.
- **Source planning is pure.** `source.text`, `source.image`, `source.video`, `source.audio`
  accept no inputs and emit existing values/assets on `out`. Empty text is valid; media sources
  may be placed unconfigured but cannot plan until an asset is selected. No placeholder hashes,
  decoding or provider calls occur. Importers/decoders validate bytes; source schemas validate
  the declared MIME modality. Node descriptors expose quick-add and mode compatibility metadata
  for the future palette and Turn-into picker.
