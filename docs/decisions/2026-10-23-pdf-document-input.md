## 2026-10-23 · Chat models read wired PDFs directly, bounded by pages and quoted by them (#591)

- **Native PDF input, not a conversion.** The Anthropic Messages API and OpenAI Chat Completions
  both take a PDF as a document part (each page's text and a picture of it), so the built-in chat
  models gain a `document-to-text` mode with a `document` role (1–5 PDFs) beside the prompt. A
  model that can't read PDFs keeps no document port; swapping a card with a wired PDF to one says
  "wire the PDF through PDF to text, or choose a model that reads PDFs" instead of a generic error.
- **Catalogs declare how a mode reads PDFs.** `document = { role, max_pages, tokens_per_page }` on
  the mode (`types::DocumentUse`). A mode with a document role must declare it, so every PDF run is
  bounded and quoted; the catalog refuses a declaration for a role the mode lacks.
- **Pages are checked at plan time, bytes in the adapter.** Planning sums the wired PDFs' page
  counts and refuses more than `max_pages` before anything is sent. A PDF nobody counted (one an
  action made) goes through, like an unmeasured source video (#179); the provider still checks
  it. The adapter caps the count (5) and the total size (22 MiB, under both APIs' request limits)
  and sends only `application/pdf`.
- **The quote estimates the pages' input tokens.** Chat quotes were output-only (input billed on
  top). A PDF is usually the bulk of the input, so the quote adds `pages × tokens_per_page` at the
  input rate of the mode the step runs (the long-prompt rate past its threshold). A PDF whose
  pages aren't counted (made by an earlier card, or never counted) counts as `max_pages`, and the
  total never exceeds `max_pages`, so the quote stays an upper bound. The measured usage is still
  what is charged.
- **5000 tokens a page** is a high per-page estimate: a dense page's text (typically 1500–3000
  tokens) plus its picture (about 1600). The quote errs toward the upper bound the confirmation
  promises, as max-token output quoting does.
- **Known gap:** like source-video checks (#179), the page check runs when the card is planned.
  A card whose prompt comes from an earlier paid card is checked only after that card has run
  (#795).
