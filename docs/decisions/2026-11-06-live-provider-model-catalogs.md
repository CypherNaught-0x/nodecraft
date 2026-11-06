## 2026-11-06 · Live model catalogs from OpenRouter and LiteLLM (#489)

OpenRouter and a LiteLLM proxy each list their own models, and those lists change faster than
the curated catalog. NodeCraft now reads them at run time and offers them in model pickers next
to the curated models. This record covers where listed models live, what they cost, and their
limits.

### What is listed

- **OpenRouter.** Chat models come from `GET /api/v1/models`: every model whose output is text
  only. They run on the `openrouter` chat adapter (Chat Completions).
  - Image models come from `GET /api/v1/images/models`, which is the only list that includes
    image-only models; plain `/models` leaves them out. They run on the new `openrouter-images`
    adapter (`POST /api/v1/images`).
  - Speech models come from `GET /api/v1/models?output_modalities=speech` and run on
    `openrouter-speech` (`POST /api/v1/audio/speech`, MP3). Models that answer only raw PCM
    (HeyGen Voice, Gemini TTS) are left out, because NodeCraft can't place PCM without its
    sample rate.
  - Video models come from `GET /api/v1/videos/models` and run on `openrouter-video`: a queued
    job that is polled, then downloaded with the key. Redirects are followed for the download
    only; the key never crosses to another host. OpenRouter documents no cancel endpoint.
  - Music and chat-audio models (Lyria, GPT Audio) are left out. Their audio comes back through
    streamed chat in a shape OpenRouter doesn't document for Lyria.
  - All these lists are public, so they are read without the key, and the keychain isn't asked at
    start-up. The key is read only when a run starts.
  - The lists are read only while an `openrouter` key account is kept.
- **LiteLLM.** Models come from `GET /v1/models` on the proxy set in `[providers] litellm_url`,
  read with the optional `litellm` virtual key.
  - `GET /model/info` adds each model's mode, price, longest reply and image input, when the
    key may read it. Embedding, image, speech and rerank deployments are left out.
  - They run on the existing `litellm` chat adapter.
- **Ids.** Listed models are `openrouter.<id>` and `litellm.<id>`, mapped into the catalog's
  alphabet: lowercase, with `:` turned into `.` and anything else outside it into `-`. The
  provider's own id is sent unchanged.
  - An id that maps onto another listed id, or onto a curated id, is dropped. Listed models
    never shadow curated ones.
- **Untrusted.** Every listing is bounded: 16 MiB per reply and 2048 entries. Each field is
  checked, and an entry that doesn't make a valid descriptor is skipped on its own. Requests
  never follow redirects.

### What it costs

- **A listed chat price** becomes the curated chat models' quote: the reply-token rate on
  Max output tokens, with prompt tokens charged once the reply reports them.
  - Per-token decimal strings are converted to exact micros, rounded up, never through floating
    point.
  - A model priced `0`/`0` with no other fee is free.
  - A model with a per-request fee or a separate reasoning rate is `by_usage`, because a token
    quote would understate it.
- **OpenRouter image models are `by_usage`.** Their price depends on the image (per output
  token, per image, by resolution).
  - A run needs the unknown-price consent and is charged OpenRouter's reported `usage.cost`.
  - The listed per-token rate goes in the description only.
  - A 502 from OpenRouter is not billed and is said so.
  - There is no idempotency key, so an uncertain submit is terminal (`retry_ambiguous_submit`
    is false).
- **Speech** listed with a per-character rate is quoted per 1000 characters, like the curated
  speech models. Speech listed with no rate is free. Speech priced by the audio made is
  `by_usage`.
- **Video.** When every listed price is a USD rate per second (by resolution, audio or input
  kind), a model is quoted at the highest rate × Duration, which is an upper bound. Any other unit
  (tokens, cents, images) makes it `by_usage`, charged OpenRouter's reported `usage.cost`.
  Durations are offered as an unsigned range stepped by the listed values' common difference. An
  uneven list can let a gap through, which OpenRouter refuses with the lengths it takes.
- **A LiteLLM model without a positive listed price is `by_usage`, never free.** A proxy's `0` or
  `null` is its own bookkeeping, not a promise. The run is charged what the proxy reports in
  `x-litellm-response-cost`.

### Where they live

- **In the generation catalog, as a new snapshot.** `models.catalog_refresh` asks the host's
  `models.catalog_fetch` service. `model_catalogs::finish` replaces the models of the listed
  providers (`openrouter`, `openrouter-images`, `litellm`) and publishes a new catalog snapshot,
  with the same rebind-and-install path plugins use.
  - Curated and plugin models are untouched.
  - An open picker rebuilds itself and keeps its search and filters. A model change under
    review is marked stale, as before.
- **A source that fails keeps its last models.** A source that is no longer set up (no key
  account, no address) loses them.
- **Nothing is saved.** Listed models exist for the session.
  - The desktop app reads the lists quietly at start when either source is set up, and again
    when the OpenRouter key account or the LiteLLM address changes. A quiet read is told only
    when it fails.
  - A project whose card names a listed model shows it as unknown until the lists are read.
  - The CLI and the browser have no fetch service yet, so `models.catalog_refresh` is refused
    there.

### Size

- The catalog's 64 MiB admission budget charges at least 64 KiB per model. About 340 listed
  models fit next to the curated ones; OpenRouter alone lists about 490.
- At most 640 listed models are offered. They are admitted in priority order until the budget
  is reached:
  1. LiteLLM;
  2. OpenRouter images, video and speech;
  3. free chat models;
  4. the rest, in listing order (newest first).
- The refresh report says how many were left out. The budget itself is unchanged.

### Model picker

The picker gains a **Provider** filter, with a count of usable models per provider, and
**Selectable only**. Selectable only is on by default and hides models that can't be used for
the targets; turning it off shows them disabled with their reason, as before. The picker also
gains **Refresh provider models**, and a resizable window with a default height
(`model_picker_height`).

### Not done

- **Browser and `serve`.** The browser has no fetch service, and `nodecraft-cli serve` doesn't
  read the lists. The CLI's `quote`, `run` and `batch` read them once per process, only when a
  card names a listed model.
- **A changed LiteLLM address.** If reading the new address fails, the old proxy's models and
  prices stay until a read succeeds. The settled charge still follows what the proxy reports.
- **Reviews and background refreshes.** A refresh that changes the catalog marks an open model
  change review stale. Picking only rebuilds the list.
- **A cache.** Listed models aren't kept across restarts.
- **Charges OpenRouter can't report back.** Chat models with a per-request fee, and speech models
  priced other than per character, aren't offered: neither reply carries its charge. A per-image
  fee drops the chat model's image input.
- **Pre-run quotes.** OpenRouter image prices aren't quoted before a run. Its per-endpoint
  `pricing` could quote models priced per image.
- **LiteLLM image generation.** LiteLLM's image deployments aren't offered.
- **OpenRouter music and chat audio.** Lyria and GPT Audio aren't offered.
- **Speech settings.** Speed and delivery instructions aren't offered.
- **Video reference images.** `input_references` aren't offered; first and last frames are.
- **Reasoning replies.** Reasoning models that spend all of Max output tokens on reasoning return
  an empty reply. The OpenAI dialect doesn't read `reasoning_content`.
