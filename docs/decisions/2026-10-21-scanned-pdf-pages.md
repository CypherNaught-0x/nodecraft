## 2026-10-21 · Scanned PDF pages are recognised by a vision model, not local OCR (#592)

- **Provider route, not a local engine.** Text is recognised by a vision-capable text model
  through the existing provider path, so the cost is quoted first, budgets apply and nothing new
  ships in the wasm build. A local pure-Rust OCR engine was set aside: the candidates need model
  weights whose licence we could not confirm as permissive, and they would add tens of MB.
- **Chat models read images.** Every built-in Anthropic and OpenAI text model has an
  `image-to-text` mode beside `text-to-text`: a prompt and 1 to 16 images (`MAX_IMAGES`, under
  both APIs' per-request limits and Anthropic's 20-image threshold for stricter dimensions). A
  text card offers an `image` port; the engine sends the prompt, then the images, as one user
  message. Anthropic images over 10 MB base64 (7.5 MB raw) are refused before sending. The quote
  stays the output price on `max_tokens`; images are input tokens, billed on top like any input.
- **Read versus recognised stays visible.** PDF to text with page headings lists every page and
  marks one that draws no text as `[Page N: no text read]`, instead of dropping it. PDF to images
  can draw only those pages (`only = without_text`), so just the scans reach the paid model. The
  recognised text is a separate card's answer, attributed to its model like any generation, so it
  is never mistaken for text read from the PDF.
- **Bounds.** Picking scans reads up to 2,000 pages' text and draws at most 64 of them; more is
  an error asking to narrow the range. A text card takes at most 16 images, so a longer scan is
  recognised in several runs (a page range each).
