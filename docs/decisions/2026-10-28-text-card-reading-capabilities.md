## 2026-10-28 · Text cards: connected media thumbnails, context-limit refusals and declared reading capabilities (#84)

- **Connected media are shown, not inferred from the run.** A text card draws the media wired
  into its non-text inputs as a row of `connected_thumb` squares at the top of its answer area,
  in port order then wire order. Each wire's value is what a run would read from it, through
  the planner's own `exec::planner::value_without_running`: a source kind's (and an Element's)
  current value, never its history, else the upstream card's usable active result (same port
  layout, not partial). A batch's items exist only once a run plans them, so they aren't shown.
  The row is view state built
  from the canvas each frame: it takes room from the answer, so wiring media never resizes the
  card or moves its ports. Thumbnails go through the shared preview cache's public request API.
- **A context-window refusal is its own message, not a new error kind.** It stays
  `UnsupportedInput` (never retried, so a retry never charges twice) but its message starts with
  `providers::CONTEXT_LIMIT` and says what to do, keeping the provider's words. Hosts that keep
  only the text recognise it with `is_context_limit_text`; the queue panel and card status name
  it "too long for the model" and give `run_jobs::CONTEXT_LIMIT_HINT`. OpenAI-compatible
  `context_length_exceeded` and Anthropic "prompt is too long" map to it. A new `ErrorKind` was
  rejected: it would renumber `ErrorKind::ALL` and every host's cause handling for one variant of
  an existing kind. The mock's `MockFailure::ContextLimit(tokens)` refuses only jobs longer than
  its window (four bytes of text a token), so tests can show a shortened prompt then running.
- **Reading capabilities are declared per mode, never assumed.** `reads = { context_tokens,
  max_output_tokens, frames = { every_ms, max }, timestamps, speakers }` on a catalog mode
  (`types::ReadingUse`). Undeclared means not offered. Only a text mode may declare it; `frames`
  is set exactly when the mode takes a video, and a text mode that takes a video must declare
  it; timestamps and speaker labels need an audio or video input. The model picker shows what is
  declared. It lives on the catalog's `ModeSpec` only, not on `nodes::GenerationMode`, so it
  adds no planner behaviour yet.
- **Known gaps.** No built-in model declares `reads` yet: the context windows of the built-in
  chat models are not confirmed here, and Deepgram's request asks for neither timestamps nor
  diarisation, so it promises neither. Planning does not yet refuse a prompt longer than
  `context_tokens` before it is sent; that needs the capability on `GenerationMode` and a
  token estimate per input. Video to text needs a provider adapter that sends video (the chat
  adapters send images and PDFs only) and is a separate provider task.
