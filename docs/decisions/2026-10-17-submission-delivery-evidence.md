## 2026-10-17 · Submission delivery evidence (#250)

- **Diagnostics do not prove absence of billing.** This supersedes the historical kind-only
  submit-refusal classifier in #227/#243. Legacy Provider and Http implementations are unknown
  before invoking their future constructors, including errors named Auth or UnsupportedInput.
- **A request is not a submission.** Opaque request scopes can prove native construction failed
  before execute; redirects share sticky possible-delivery state. Only a runner-owned audited
  operation with no earlier taint or unfinished child can finalize whole-submit nonacceptance.
  Proofs cannot be cloned, publicly minted or reused across children/invocations. Cancel/drop
  after invocation remains unknown; a handle independently establishes acceptance.
- **Useful narrow opt-in.** LLM submission tracks its local construction and single request.
  Responses, status/parsing/body/sink errors stay unknown. Comfy's multi-request path and browser
  fetch remain conservative. No generic HTTP refusal attestations, billing wire migration or
  new retry/idempotency policy. Existing prior uncertainty and captured-attempt ownership win.

