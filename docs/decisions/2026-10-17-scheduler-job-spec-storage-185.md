# 2026-10-17 · Scheduler job-spec storage (#185)

- **Borrowed preflight precedes topology and mutation.** Aggregate forward slots and lane/key
  lengths are checked across existing plus incoming jobs. A separate hard input-capacity policy
  allows ordinary allocation slack; accepted specs use boxed slices and strings, so retained
  accounting has no hidden spare capacity.
- **Reverse edges are unique ordered sets.** Their live entries are bounded by forward slots,
  and requeue no longer scans a wide vector for membership. Ready selection still uses original
  addition order; sibling terminal events follow job-ID order. Forget rebuilds only affected
  surviving dependencies and releases spec charges accurately.
- **Concurrency configuration and outcomes remain separate.** The infallible default
  constructor and public `JobSpec`/`Limits` remain compatible; optional lower storage ceilings
  use a separate constructor. See `scheduler-storage.md` for deliberate defaults and boundaries.
