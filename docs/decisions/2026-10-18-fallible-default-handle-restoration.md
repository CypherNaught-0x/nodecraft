## 2026-10-18 · Default handle restoration reports refusal (#365)

The legacy/default ProviderRunner restoration methods return a typed refusal instead of
silently skipping an unaddressable provider. Validation constructs the same bounded default
target before touching runner state. Empty and arbitrary bounded adapter strings remain valid;
legacy key normalization and saved generation behavior are unchanged. This does not broaden the
scheduler Start contract or adopt the named restoration method's stricter key policy.

A refusal owns the exact accepted handle and formats only the public-safe cause. The host keeps
its persisted provider/base/generation tuple until success, recovers the handle and retries
restoration explicitly after repairing the address metadata. Refusal never authorizes deleting
the persisted accepted identity, remote cancellation or a replacement submit. Named restoration
continues to require its captured typed endpoint binding; lane strings cannot mint that authority.
