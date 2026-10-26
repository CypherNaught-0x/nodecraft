## 2026-10-24 · Hosted ComfyUI is a metered server profile (#16, #865)

Comfy Cloud's v2 transport already existed, but the desktop's Workflow node produced a different
job shape and the servers panel constructed only local adapters. Treating a hosted address as a
local server would also inherit the local catalog's free price.

Keep the existing Workflow catalog entry, node mapping and server selection command. A settings
profile explicitly selects `local` or `cloud`; missing tags preserve existing local profiles,
and unknown tags are ignored. Cloud profiles use the fixed official origin and the `comfy-cloud`
keychain account. Their adapter presents the logical `comfyui` identity while delegating to the
feature-gated Comfy Cloud transport. This preserves named server concurrency and existing card
selection without duplicating models or leaking a key to a configurable URL.

Capture the hosted billing policy once in the immutable physical provider binding. Quote and
admission both mark its jobs unpriced, including custom catalog prices, independently of settings or a successful
node-list fetch. Finite budgets reject them; uncapped runs require the existing explicit partial
quote consent. Routing changes invalidate quote tokens and prepared work. Actual cloud GPU
charges cannot yet be settled from this API, so do not invent a price or a zero charge.

Reuse local Workflow parsing, value overrides, output pruning and preset expansion in the cloud
adapter. Check against the host's catalog before uploads, encode upload tags as a JSON array,
and order returned output nodes like local ComfyUI history. Partner nodes remain unsupported.
The v2 API rejects reuse of an idempotency key rather than replaying a response. The hosted
profile therefore preserves the adapter's prohibition on retrying an uncertain paid submit.

References: [provider behavior and setup](../providers.md#comfy-cloud-workflows),
[official API schema](https://github.com/Comfy-Org/docs/blob/main/openapi-v2.yaml), and
[SDK idempotency contract](https://github.com/Comfy-Org/docs/blob/main/development/api-development/sdks.mdx).
Offline contracts and actual engine/UI fixtures cover the profile; the explicitly opted-in live
smoke only reads the catalog. No provider calls are made by ordinary tests.
