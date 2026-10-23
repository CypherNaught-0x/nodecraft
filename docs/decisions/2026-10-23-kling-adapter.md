## 2026-10-23 · Kling runs on its own task API, keyed by an API key or a signed pair (#19)

- **Direct, not through an aggregator.** Higgsfield and fal already offer some Kling models; this
  adapter talks to Kling's own task API so a Kling account works without a second vendor, and so
  multi-image references and end frames are available.
- **One keychain entry, two key shapes.** Kling issues both plain API keys and access key / secret
  key pairs. The `kling` account holds either; a value with one colon is a pair, joined `ak:sk`
  like Higgsfield's. The pair signs a short HS256 token per request (`hmac` + `sha2`, already in
  the workspace) rather than adding a JWT crate. The clock comes from `web-time`, pinned, because
  `std::time::SystemTime::now` panics in the browser.
- **The legacy v1 endpoints.** The newer Omni endpoints are documented only behind pages that
  refuse scripted reads; v1 (text2video, image2video, multi-image2video, images/generations) is
  documented consistently by independent mirrors. Moving to Omni is a follow-up once it can be
  checked.
- **No automatic replay; `external_task_id` instead.** The create is the paid step. Kling has no
  idempotency header, but refuses a second task with the same `external_task_id`, so video creates
  carry a hash of the job's idempotency key. The image endpoint takes no such id, so lost image
  replies are reported as possibly billed, like every other create.
- **By usage pricing.** Kling bills prepaid units by model, mode and length; a USD price would be a
  guess, so the catalog lists the models as by usage and budgets treat them that way.
