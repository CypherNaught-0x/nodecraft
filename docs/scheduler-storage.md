# Scheduler job-spec storage

`Scheduler` bounds retained job specifications independently of concurrency `Limits` and pending
commands (#184). `Scheduler::new` remains infallible and uses the hard defaults below.
`Scheduler::with_storage_limits` accepts lower `StorageLimits`; invalid ceilings return
`InvalidStorageLimits`. Zero is permitted and prevents the corresponding nonempty storage.

| Dimension | Default and hard ceiling |
|---|---:|
| Retained jobs (unchanged) | 100,000 |
| Dependency entries per submitted job (unchanged) | 4,096 |
| Aggregate forward dependency slots | 1,000,000 |
| Aggregate job lane + idempotency-key UTF-8 bytes | 16 MiB |
| One job lane | 2,048 bytes |
| One idempotency key | 1,024 bytes |
| Raw requeue entries, including duplicate IDs | 100,000 |

One million dependency slots supports an ordinary 10,000-node workflow with an average of up
to 100 dependencies per job. The representative regression executes 10,000 nodes with eight
shared upstream dependencies per node. The existing 100,000 independent-job regression remains
valid. Keys and lane names retain their exact bytes: there is no truncation, case folding or
idempotency-key rewriting. The existing recommendation for provider-portable keys under 64
characters remains advice; this storage ceiling does not imply all providers accept 1,024 bytes.

## Admission and retained representation

Before allocating ID/topology maps, deduplicating dependencies or modifying scheduler state,
`add` checks the raw job count and outer vector capacity, then borrows the incoming specs.
Logical retained charges use existing plus incoming dependency and UTF-8 string **lengths**.
Lower `StorageLimits` apply to these lengths, independently of incoming allocation slack.
Accepted specs move into boxed dependency slices and boxed lane/key strings with no spare
capacity field. Duplicate dependencies still consume forward slots and remain in the
specification, but execution and reverse indexes normalize pairs.

A separate hard incoming allocation policy allows four times the hard logical ceiling: 400,000
outer vector slots, 4,000,000 aggregate dependency slots, 64 MiB aggregate lane/key capacity,
8,192 bytes per lane capacity and 4,096 bytes per key capacity. Checked accumulators reject
excess with `InputCapacityBudget` before topology scratch. The allowance is independent of
lower retained limits: a ten-byte key with 2,048-byte capacity is normalized and accepted if
its length fits. A reused key or lane buffer above its four-times-hard capacity ceiling still
returns `InputCapacityBudget` even when its text fits. Before submission, callers can use
`shrink_to_fit` and check the resulting capacity, or reconstruct a compact buffer. Ordinary push-built dependency vectors fit, including the regression with
10,000 jobs, 895,905 raw dependencies and over one million allocated slots. The caller already
owns these allocations; this policy does not prevent caller allocations or estimate process
memory. Count/capacity failures precede logical budget and graph-semantic errors. A rejected
batch leaves jobs, indexes, sequence, counters and events intact. Errors identify only the
dimension, never submitted lane or key text.

Reverse edges use `BTreeSet<JobId>` per upstream job. The number of unique reverse edges cannot
exceed charged forward slots, including cross-batch edges and requeue. Set insertion avoids the
old repeated linear membership scan during wide requeue; sibling traversal is deterministic by
job ID. Ready-job selection still follows original addition sequence, independent of set order.
Set node overhead is bounded by edge/job counts, not claimed to equal eight bytes per edge.

The forward slices occupy at most 8 MiB of JobIds. Reverse sets, topology users/deduplication
scratch, dependency clones for transitions, and graph-derived cancellation/propagation traversal
storage have bounded constant-factor allowances derived from one million edges and 100,000 jobs.
A direct `Scheduler::cancel(CancelScope::Jobs(ids))` caller can supply arbitrarily many duplicate
roots; that caller-supplied root capacity and its initial collection are outside this contract.
Driver command admission bounds submitted cancel payloads separately (#184). Metadata also has
bounded copies: ready/running map lane keys, temporary state-transition lane clones, and emitted
`Start` lane/key values. Each map holds at most one key per active lane; aggregate lane lengths
are no greater than retained job lane bytes. Returned `Start` values are caller-owned; retaining
unlimited historical outputs is outside this scheduler storage contract. These are structural
bounds, not an allocator-exact process-memory estimate.

## Requeue and forgetting

Requeue checks raw entry count, every key length and aggregate source key length before
building target/key maps. All entries, including overwritten duplicate keys, must meet the
individual logical ceiling and source key lengths together cannot exceed the metadata ceiling.
This borrowed API neither retains nor charges caller spare capacity. Input-budget errors precede
unknown-job or state errors; callers must not depend on a semantic error hiding invalid input. Duplicate IDs
keep the existing last-key-wins behavior. Final accounting subtracts each old target key once
and adds each winning replacement once; the resulting retained total must fit before any
mutation. Temporary replacement allocation may overlap the old boxed keys during consumption,
so the retained metadata ceiling is not a peak-allocation claim.

Terminal jobs retain their spec charges until forgotten. `forget_finished` removes their reverse
edges and rebuilds only boxed forward slices containing forgotten IDs, then recomputes charges
once. With no finished jobs it returns immediately; unaffected slices retain their allocation. It does not pretend `Vec::retain` freed spare capacity. Repeated child waves around a
long-lived root therefore cannot accumulate historical reverse-vector allocations. Dependency
and metadata counters do not require whole-graph scans during normal start/report/retry paths.

This bounds job-spec storage only. Arbitrary caller-provided concurrency configuration
(`Limits.per_lane`, including its string/map capacities), pending driver commands, event queues,
job error strings/outcomes and caller-owned outputs have separate ownership and limits. No outcome
or event is silently discarded to enforce these budgets. Paid retry and cooperative cancellation
semantics, including the fan-in settle-only-when-ready correction, remain unchanged.


## Provider endpoint limits and usage export

Provider lanes use canonical target encodings (`p:<adapter-hex>:d` for the default endpoint,
`p:<adapter-hex>:i:<instance-hex>` for a named endpoint). The literal adapter name `local` is
separate from the real local Source/Action lane. Configure endpoint concurrency through
`Limits::set_target_limit(&target, limit)` rather than a bare adapter-name lane string.
Scoped usage CSV adds `provider_instance`: an empty value is the legacy/default endpoint,
while a named value identifies its endpoint UUID. This column does not imply old records
contained a named instance or that a lane string is restoration authority.

Authorized runs derive each queued job's known, unknown or free price from their retained
admitted quote; a host cannot replace it with an unbudgeted price argument. The accepted
amounts are also the run's cumulative ceiling per currency (#366,
[decision](decisions/2026-10-18-run-ceiling.md)): every new priced reservation (deferred
fan-out, concurrent jobs, retries under a new key, requeues and replacements) is debited at its
estimate under the usage log's lock and refused before the provider when it would pass the
accepted amount. Releases on no-delivery evidence credit it back; unresolved and completed
charges keep their debit. Unknown-price work stays limited to steps the quote listed as unpriced
under partial consent, and the host budgets apply as before.
