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
  `GenerationMode` carries the mode's `reads`. The estimate is a low one: the composed prompt's
  bytes over six (`types::BYTES_PER_TOKEN_AT_MOST`; tokenizers average about 2.5 to 4 bytes a
  token on ordinary text), and media count for nothing, so it never refuses a prompt that might
  fit and only catches what is clearly over. The message starts with the shared
  `types::CONTEXT_LIMIT` text (now defined in `types` and re-exported by `providers`), so hosts
  label it "too long for the model" just like a provider's refusal, and names the model; the
  quote's blocked reason names the card. Anything closer to the limit is left to the provider's
  own refusal, which is already mapped to the same message.
- **A video longer than the declared frames is refused while planning** when its length is
  known, as audio past its limit is. A video of unknown length goes through.
