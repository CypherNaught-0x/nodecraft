## 2026-11-04 · Text cards read video through Gemini and refuse prompts clearly past the window (#84)

Closes the three gaps [the reading-capabilities record](2026-10-28-text-card-reading-capabilities.md)
left open.

- **Video goes through the Interactions API, inside the existing `gemini-llm` adapter.**
  Google's OpenAI-compatible endpoint, which the Gemini text card uses, documents text, image
  and audio parts and no video. Rather than a second Gemini text model or a new provider, the
  adapter sends a job that carries a video as one stateless Interactions request and keeps
  every other job on the compatible endpoint, so a card keeps one model and gains a
  `video-to-text` mode. The request is not streamed and is never replayed once sent, as for the
  other Interactions adapters.
- **Only documented limits are declared.** Inline video under 20 MB of request (the guide's
  lower figure, over its table's 100 MB), at most ten videos, the reference's MIME list, one
  frame a second by default, and three hours at the default resolution for a 1M-context model
  (10,800 frames). The one inference is sending NodeCraft's `video/quicktime` as Google's
  `video/mov`. Files API uploads, clipping, custom frame rates and media resolution are not
  offered, and timestamps and speaker labels are not declared. Pages and dates are in
  [`providers.md`](../providers.md) and the catalog.
- **Built-in text models declare `reads` where a public page states the numbers:** Claude Opus,
  Sonnet and Haiku 5.5 (1M window, 128K output), GPT-6.1 Sol and GPT-6 Luna (1,050,000, 128K),
  GPT-4.1 mini (1,047,576, 32,768) and Gemini 3.8 Flash (1,048,576 input, 65,536 output; Google
  states an input limit, so that is the window). The Azure deployment, the Hugging Face routed
  model and the transcription models stay undeclared: their limits depend on the deployment or
  route, or are not published as a context window.
- **Planning refuses a prompt clearly past the window, before quoting or sending.**
  `GenerationMode` carries the mode's `reads`. The estimate is a low one for ordinary text: the
  composed prompt's bytes over six (`types::BYTES_PER_TOKEN_AT_MOST`; Anthropic documents about
  2.5 characters a token), and media count for nothing, so it only catches what is clearly over.
  The message starts with the shared `types::CONTEXT_LIMIT` text (now defined in `types` and
  re-exported by `providers`), so hosts label it "too long for the model" like a provider's
  refusal, and names the model; the quote's blocked reason names the card.
- **It never fires for today's built-in models.** A card's composed prompt is at most 1 MiB
  (`compose_text`), about 175,000 tokens by this estimate, and every declared built-in window is
  over a million tokens. The check matters only for models declaring windows under about 175K
  tokens (custom catalogs, smaller models later). Built-in models rely on the provider's
  refusal: the Anthropic and OpenAI-compatible routes map theirs to the context-limit message.
  The Gemini video route can't: Google documents no token-limit error for the Interactions API
  (its error list has no such code), so a refusal there reports the HTTP status and Google's
  documented error `code`, and video tokens aren't estimated.
- **Video type and size are refused at quote time.** The mode declares `reads.video_types` (the
  reference's list plus NodeCraft's `video/quicktime`) and `reads.video_bytes` (14,990,000 for
  Gemini, what fits a request under 20 MB after base64). Planning refuses an unlisted type;
  `run.quote` and `run.start` refuse wired videos whose stored sizes total more, before anything
  is reserved or sent. The adapter still checks the exact request.
- **A video longer than the declared frames is refused while planning** when its length is
  known, as audio past its limit is. A video of unknown length goes through.
