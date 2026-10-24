## 2026-10-20 · Text generation is a second catalog kind, not a new node family (#84)

- **One contract, two outputs.** `generation.text` is the same `GenerationKind` as
  `generation.image`, built with a different output modality. Admission, model choice and
  swapping, the shared parameter contract, quoting, idempotency and `run.record` stay one code
  path. The catalog snapshot builds one kind per output its modes make, from the modes with that
  output, and looks a node's definition up by the node's own kind.
- **A model belongs to a kind through its modes.** A model with text modes is a text card's
  model; one with image modes is an image card's. A model with both is defined, and charged
  against the metadata limit, once per kind. `GenerationCatalog::model` and
  `validate_destination_wiring` take the kind. A model swap applies to cards of one kind and
  refuses a mixed selection, since one destination can't serve both.
- **One answer per run.** A text card's `out` port is `Text`, not a list: a run records one
  answer. Several answers come from running again, which the history strip already keeps.
- **Connected text replaces the local prompt.** As on image cards
  (`TextPrecedence::ConnectedReplaces`). The local prompt stays in the document as the draft, so
  disconnecting brings it back.
- **Wording.** Image errors keep their exact text (callers and tests match them). Text cards get
  the same messages with "text".
- **Not yet.** These are follow-ups:
  - connected media for image/video/audio-to-text, which is refused before submission today (#502);
  - builtin text-model catalog entries with verified prices (#503);
  - streaming answers (#504);
  - `@node` mentions (#505) and `#element` mentions (#104).
