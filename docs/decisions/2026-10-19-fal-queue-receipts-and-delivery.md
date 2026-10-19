## 2026-10-19 · fal queue receipts, delivery and uncertainty (#11)

- **Literal routes.** Allowlisted model roots and coherent returned route families are compared
  before URL normalization. A complete bounded receipt with a safe ASCII request ID separately
  authorizes one canonical local model-root cleanup PUT after later admission refusal. A rejected
  returned URL is never used. Cleanup reports the original refusal, safe ID and actual disposition.
- **Best effort is not a billing proof.** Accept only the documented matching cancellation pairs
  202/CANCELLATION_REQUESTED, 400/ALREADY_COMPLETED and 404/NOT_FOUND. Other or malformed replies
  remain errors. Unlike the SDK (which raises on 400/404), NodeCraft treats those matching replies
  as best-effort outcomes; none releases uncertain spend or authorizes paid replay. See the
  [official queue contract](https://fal.ai/docs/documentation/model-apis/inference/queue), checked
  2026-10-19. An ambiguous create never retries. Accepted handles resume instead.
- **No implicit transport authority.** Queue requests alone receive auth; HTTPS fal.media CDN
  downloads never do. Redirects are refused. X-Fal-No-Retry and x-app-fal-disable-fallback suppress
  provider retries/fallback but do not prove billing deduplication. Queue polling defaults to
  2,000 ms. Bounded Retry-After integer
  seconds can defer retries up to one hour without replaying an ambiguous create.
- **Paid bytes inside the limit survive.** Admit body length, headers and Content-Length;
  allocator spare capacity is not a wire rejection rule. The providers worksheet enumerates
  logical live payloads, not transport capacity, codec-private workspace or RSS. WAV/MP4/GLB
  checks admit bounded containers, not full audio/video/mesh playback correctness.
- **Compose with host readiness.** The existing provider factory uses the `fal` keychain
  account and reports missing/locked keys before routing. Construction sends no requests. This
  adds no paid-run host or canvas media-input resolver.
- **Unknown is not free.** Four catalog models have unknown ByUsage pricing; Trellis has its
  documented dated estimate, not an invoice. Optional billable units are not token counts or
  Money and remain separate follow-up #437. No authenticated or paid call was used for these
  synthetic contract and loopback controls. See [provider bounds](../providers.md).
