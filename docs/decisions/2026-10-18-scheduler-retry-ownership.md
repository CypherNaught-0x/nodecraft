## 2026-10-18 · Scheduler retries outlive proposed provider registration keys (#290)

A same-provider registration replaces the future payload, but its proposed key does not retire
an already scheduled retry. Keep one private scheduler lease inside the registration, separate
from its payload incarnation. An admitted Start supplies the authoritative base key; its invocation
serial guards lease retirement and held accounting metadata. Same-key retries preserve provider
failure generations and accepted handles. A fresh Start or provider switch retires the obsolete
scheduler identity, without removing identities still owned by other jobs or active invocations.

Terminal outcomes, cancellation and dropped tasks retire only their captured scheduler lifetime;
old callbacks cannot erase a replacement's lease or deferred cleanup. Host settle remains accounting
cleanup, not forget: a same-key registration can still reference a handle after its lease ends.
Forget removes that registration reference too. Driver task Drop reports a nontransient failure;
the host must still settle accounting explicitly. No remote cancellation or nonbilling is inferred.
A no-handle local failure cannot invalidate another job's accepted shared handle; explicit remote
completion/failure remains authoritative for its exact generation.

Strict accounting permits remain bounded, exclusively owned capabilities. Re-registration can keep
an inactive permit for the scheduler's old attempt; an admitted replacement retires obsolete ones.
A refused replacement does not erase the legitimate retry lifetime. This does not change captured
provider retry policy, charge conservation, completion delivery or accounting-only repair semantics.
