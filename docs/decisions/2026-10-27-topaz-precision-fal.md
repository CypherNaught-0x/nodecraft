## 2026-10-27 · Bound Topaz Precision to one priced output block through fal (#37)

- Reuse the existing fal queue, stored fal key, uncertainty ledger, canonical saved handles
  and cancellation. Select only `topaz/upscale/image/precision` Standard V2; do not add a
  second Topaz credential or invent support for the direct API or other endpoint families.
- One upright PNG/JPEG/WebP input is limited to 1 MiB and 1024 pixels per axis. Expose only
  the integer upscale factor 1–4, default two. Fix PNG, no crop and safety on; omit optional
  enhancement controls so documented service defaults apply. No text or seed is sent.
- The [published price](https://fal.ai/models/topaz/upscale/image/precision) is 0.08 USD per
  started 24 megapixels of output. The largest admitted geometry, 4096², is below one block;
  quote 0.08 USD per result. Variations remain separate priced requests. Persist the expected
  geometry with the handle and verify the result, so resume cannot discard that contract.
- Topaz alone admits 256 MiB PNG outputs; legacy fal download limits remain unchanged.
  Do not charge a host whose classic 16-MiB asset store cannot retain these results: the
  final executed quote identities require streaming asset storage before consent or creation,
  including deferred/inlined work. Browser submissions also refuse locally. Shipping desktop
  and native CLI run storage already support streamed assets; no storage cap is widened here.
- Native recording, resumed results and held-result placement convert, hash and seal outputs
  above 16 MiB on the existing runtime workers. The frame only adopts sealed receipts; it
  never falls back to a large synchronous import. Held-record encoding and writing also run
  on a worker, retaining the old store identity and original bytes if persistence fails.
  Pending local result work delays desktop close without blocking frames. The shared 256-MiB
  output bound and exact hash checks remain; small/classic imports retain their limits. A real host regression
  exposed the former convenience call even on native storage; large results now reach history
  and can be retried from held state and saved/reopened without another paid request.
- Contracts and price were checked 2026-10-27 against the primary
  [API](https://fal.ai/models/topaz/upscale/image/precision/api) and
  [queue documentation](https://fal.ai/docs/documentation/model-apis/inference/queue).
  Offline authored fixtures and real host tests establish client behavior, not paid service
  quality. Live generation stays explicitly opt-in and was not performed for this change.
