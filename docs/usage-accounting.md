# Usage accounting and historical ownership

`UsageLog` retains exact reported project/session totals separately from admission totals.
An old unmarked log can contain a positive named open charge and earlier-session spend
without enough evidence to decide which owns the open amount. This is uncertainty about
historical session ownership, not a claim that provider execution or billing is unknown.
Loading preserves that log
for inspection; settling or releasing the uncertain provider/key returns an error. It does
not guess ownership from the amount or promote an uncertain entry during save/load.
Unrelated established charges remain usable. Session admission adds the original uncertain
open costs to the named reported total and clamps that upper bound to project spend.
Project admission uses exact project spend. `budget_view` exposes both values explicitly.

Wire marker 1 means established ownership. Marker 2 additionally retains group evidence and
uncertain identities, including entries evicted from the ordinary FIFO. The original named
floor plus established open charges must remain covered. Cross-group evidence for one
currency must agree. A marker-2 file cannot be loaded by the older strict wire reader, nor a marker-3 file by a
marker-2 reader.
`write_csv` exports ordinary listed entries; `write_quarantine_csv` separately exports retained
uncertain entries, including those outside the FIFO. Neither export invents a currency or
turns an unknown cost into zero.

A person can resolve a group after checking their own provider records (#325):
`usage.ownership` lists the groups, then one group's evidence, totals and charges with a token;
`usage.reconcile_ownership` (human only) states which total holds each charge, as a default
holder plus at most 1,000 exceptions. A large group is decided in stages: `only` lists at most
1,000 charges for one stage, the rest stay quarantined under the same evidence, and each stage
is reviewed against a fresh token. The decision is refused when the token is stale, when
named-held charges exceed the named total at import, when earlier-held charges exceed what the
earlier total held at import or holds now (less what folded sessions' listed charges need
from it), or when the named total would not cover its open charges. `dry_run: true` reports the
record and resulting totals first. Earlier-held money moves to the named session with the project unchanged; the charges
become established and settle or release normally afterwards. Reconciliation itself settles
nothing. Marker 3 adds the immutable audit records, one per stage. On load, a group's stages
carry its evidence unchanged, their named-held sum stays within the named total at import (the
group's named floor shrinks by it while the rest is quarantined), and the staged charges plus
the still-quarantined ones equal the open amount at import, each charge once; see
[the decision](decisions/2026-10-18-ownership-reconciliation.md).

In the desktop app, "Review quarantined charges" in the palette opens a panel for this (#362).
It lists the groups, shows one group's evidence, totals and charges, and requires a preview
before Confirm is enabled. The run host applies a confirmed decision to a copy of the usage log
and saves the copy in the run journal before the live log takes it. A failed save therefore
leaves both unchanged, and the same review can be confirmed again. See
[the decision](decisions/2026-10-20-ownership-host.md).

A reconciled charge is established again, but no run is left to settle it: the run that held it
ended in an older NodeCraft. A person states its outcome after checking their provider's records
(#332). `usage.unsettled` lists these charges, each with a token over the charge as reviewed;
`usage.resolve_charge` (human only) records it as `settled` at the billed amount, as an actual
charge, or `released` because it was never billed, which takes it out of the totals. Releasing
an unresolved charge is allowed here, unlike the automatic path, because a person confirmed it.
A changed charge is refused as stale, and `dry_run: true` reports the session and project totals
afterwards. Each decision is final and kept as an immutable audit record (marker 4), at most one
per reconciled charge. A charge a saved run still follows is refused, since that run settles it
when it resumes. The desktop lists these charges below the quarantined groups. See
[the decision](decisions/2026-10-20-charge-resolution.md).

Unknown-price jobs reserve metadata capacity before invoking the provider. A non-cloneable
permit captures a live ledger identity, exact provider/key, metadata and one serial. Its
lifetime owns the admitted slot; ordinary mutations cannot steal it. Dropping a permit only
marks abandonment. The next ledger operation reclaims its slot. Cloning/loading a ledger
creates a new live identity and omits active holds while retaining finalized actual entries.
Replacing the object behind the same shared lock therefore invalidates old permits.

Finalization moves admitted metadata into an inline actual entry without allocating, on
success or typed failure. It uses checked integer arithmetic rather than allocating general
errors. Ordinary transactions may later fold finalized entries into the base representation.
All totals, ordered entry iteration, serialization and exports include finalized entries.
Logical limits bound retained entries, text, pending identities and metadata shells; these
are not allocator-RSS or whole-pipeline out-of-memory guarantees. Native-only dev tests use
the exactly pinned MIT/Apache-2.0 `allocation-counter` 0.8.1 safe measurement API. Its installed
global allocator counts per thread; a parallel-allocation control checks test isolation.
Production and wasm dependency graphs do not include this instrumentation.

`ProviderRunner` delivers the original `Completed` once in `CompletionDelivery`, with an
explicit accounting result. An accounting failure retains the output and original hold,
then reports a non-transient scheduler failure. A supplied retry capability performs only
accounting against that captured identity/report: it never invokes a provider, restarts a
scheduler job or redelivers output. A currency mismatch is a failure, not an estimated-currency
actual charge. No reported or estimated cost is explicitly unknown, not free. A job whose
provider reports tokens but no cost, in an authorized run of one model with catalog token
rates, is charged those tokens at the rates (#595); otherwise it is charged its estimate. Existing
provider idempotency and handle behavior remain separate from this accounting capability;
this does not provide durable restart recovery or fix handle re-registration issue #290.

## Retention worksheet

The ordinary listed limit is 100,000 entries/32 MiB text; established pending entries
add at most 10,000 entries with four 512-byte fields each. Quarantine and metadata holds
share that combined 110,000-entry and 32 MiB + 20,480,000-byte ceiling. Moving a listed
uncertain entry into quarantine frees no combined capacity. Quarantine digests are full
32-byte identities; group session strings share an immutable owner across currencies.
Original open evidence remains charged even after an uncertain entry leaves the FIFO.

Metadata shell and identity arrays have at most 10,000 actual backing slots each.
Their capacity is checked after fallible exact reservation, before installing the new
buffers. The retained header bound is
`capacity(shells) * size_of::<Shell>() + capacity(indices) * size_of::<([u8; 32], u64)>()`;
statistics retain these bytes through vacancy. Admission can temporarily hold old plus
new arrays, each bounded by that same limit. Text is copied once into bounded owned
fields; caller-owned spare String/Vec capacity is compacted on admission. Finalization
moves those strings and writes inline Money, without growing either array.

Runner active invocation/serial tables and inactive metadata transfers are capped at
10,000 attempts. Each bounded provider/model identifier is at most 512 bytes, and the
scheduler key is at most 64 ASCII bytes. Registration incarnation proof is attached to
the existing registered job, not an independent map. Old active incarnations survive
only through bounded invocation guards. Existing host registration/handle inventories
retain their preexisting host-controlled lifetime contract; this change does not claim
a new global memory quota for every ProviderRunner payload.

## Current run-command resources

`BudgetState::from_usage(budgets, log, session)` constructs the host resource using conservative
admission spend, including finalized Actuals. Hosts that populate its public `spent` field
manually remain responsible for supplying that admission view rather than raw reporting totals.
`UsageSnapshot` exports ordinary listed rows: `listed` includes finalized Actual overlays, and
`write_csv_rows` paginates the same reserved-sequence merge as full listed CSV, skipping held
shells. An evicted uncertain row is retained in marker2 JSON and the separate quarantine CSV;
it does not masquerade as a listed row or consume a listed pagination offset. The existing
command page-size policy is unchanged by this integration.

## Scheduler requeues and provider generations

A fresh scheduler key abandons only obsolete inactive metadata owned by that JobId.
An older active invocation keeps its own permit until its outcome; unrelated JobIds retain
their holds. At the 10,000-attempt cap an exact inactive retry transfers its existing permit
rather than requesting another slot. Claimed-but-not-yet-transferred representations count
once. New identities still refuse before provider callbacks; foreign ledger retries preserve
the original capability for explicit restoration.

The landed provider-generation API remains authoritative: `Sent` reports actual sent key
and generation, `restore_generation` restores that generation, and accounting uses the sent
key while registration retains the scheduler base. A provider-reported failure advances via
`max(existing, captured_generation + 1)`; a lost reply resumes the same sent key. Strict active
exclusion applies to provider plus sent key, including runs without budgets, so the earlier
concurrent shared-key acceptance test is intentionally replaced by Busy/no-duplicate proof
and a controlled public restore/late-older-failure ordering regression. That host-restore
scenario is not claimed to be ordinary normally admitted concurrent same-key execution.
Both base and sent identities are retained in bounded attempt records; the existing strict
runner scheduler-base limit remains 64 ASCII bytes. The standalone `sent_key` normalization
API retains its broader deterministic input support.

## Delivery evidence and strict accounting

Provider failures use the landed whole-submit delivery proof boundary. A fresh reservation
can be released for pre-invocation cancellation or audited nonacceptance; Auth, UnsupportedInput,
Budget and other diagnostic names alone never establish that nothing was accepted. A returned
handle or earlier ambiguous submission keeps its monetary hold even if a later attempt proves
a local failure. Strict ledger ownership and live-capability checks still apply independently.

A currency-free metadata shell is only retained accounting capacity: reclaiming it after a
failed unknown-price invocation proves neither billing nor nonbilling and records no zero-cost
Actual. Completed output still carries its typed accounting result exactly once. See
[provider delivery contracts](providers.md) for audited LLM/native opt-in and conservative
legacy, Comfy and browser paths.
