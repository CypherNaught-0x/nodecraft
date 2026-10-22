## 2026-10-22 · Quote known character-priced inputs at run admission (#673)

- **Count planned job inputs, rather than the card's prompt setting.** This includes wired
  source text, nested text lists and resolved mentions. A deferred step remains unknown; an
  inline prompt is never substituted for text an upstream job will produce.
- **Only implicit character pricing changes.** A `thousand_characters` rule with neither
  `count` nor `default_count` uses the known text. Explicit counts, token prices and other
  measured usage keep their existing behavior. Catalog parameter validation precedes counting.
- **Count what the adapter sends.** ElevenLabs sends one trimmed prompt; the quote uses the
  same Unicode scalar count and refuses to infer a price for multiple prompts. Other providers'
  text retains its whitespace. Non-text scalar conversion remains unknown.
- **Round up and reserve conservatively.** Money uses checked integer ratios, rounded to a
  micro-unit. A fanned step reserves the dearest known job for each item, following the existing
  source-duration and provider-quote policy. A single known job has its exact character quote.
- **Bound the additional traversal.** Existing job measurement checks depth and item limits
  before recursion. A quote-wide input-byte allowance and a per-step job cap bound counting;
  nothing is copied or submitted to a provider to calculate the price.
