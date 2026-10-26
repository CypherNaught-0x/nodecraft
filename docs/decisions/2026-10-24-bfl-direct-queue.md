## 2026-10-24 · Direct Black Forest Labs queue boundaries (#33)

Status: Accepted

## Context

Issue #33 adds direct FLUX access. BFL's global API routes jobs to clusters and requires
clients to retain the returned polling URL. Finished images use short-lived signed URLs
on region-specific delivery hosts. The API documents no create idempotency key or cancel
endpoint. Its credit and pixel pricing is not a guaranteed monetary quote in NodeCraft.

## Decision

Expose the fixed `flux-2-klein-4b` text-to-image endpoint behind a `bfl` provider feature.
The typed adapter implements the existing Provider queue protocol; the shared driver
owns scheduling, resume, budgets and cancellation state. A paid create is never replayed
after an uncertain response. A valid handle keeps its task id, polling URL and requested
dimensions, with no credential. Validate it again before every poll.

Only HTTPS `api.bfl.ai` or `api.<single-region-label>.bfl.ai`, path `/v1/get_result`,
with exactly the matching `id` query is authenticated. Downloads accept only HTTPS
`delivery.<single-region-label>.bfl.ai`, use no credential, and never follow redirects.
Userinfo, non-default ports, fragments and key echoes are refused. Regions are not
hard-coded because BFL adds and removes clusters. Metadata is capped at 64 KiB and PNG
downloads at 32 MiB. Decode within 64 MiB codec allocation and validate requested dimensions.

The catalog offers dimensions through 1984 pixels in steps of 16, so every allowed pair
fits within the local four-million-pixel limit. Direct calls may use an axis of 2048 if
the combined geometry still fits. No image editing, webhooks, custom endpoints or service
safety overrides are exposed. Other BFL models remain available through existing aggregator
adapters where their own catalog and contracts support them.

Prices stay explicitly unknown, requiring the existing unpriced-job consent and refusing
finite-budget admission. Credit-valued response fields never become dollar amounts.
The built-in browser transport cannot guarantee no redirects and returns unsupported.

## Consequences

The desktop can resume an accepted job without paying for another create and immediately
stores the validated PNG in its own asset store. Local cancellation cannot prove a remote
stop or refund; errors say so. The initial direct integration is text-to-image only.
Wire fixtures and actual host tests establish contracts, not live service availability.

## Sources

- [BFL generation and delivery](https://docs.bfl.ai/quick_start/generating_images)
- [BFL OpenAPI](https://api.bfl.ai/openapi.json)
- [BFL pricing](https://bfl.ai/pricing?category=flux.2)

Verified 2026-10-24. No model code, weights or third-party workflow assets were copied.
