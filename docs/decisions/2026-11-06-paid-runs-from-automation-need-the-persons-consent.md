## 2026-11-06 · Paid runs from automation need the person's consent, per channel (#1079)

Until #1079 automation could quote a run in the desktop app but never start one. Provider routing
was set up only when a person pressed Generate, so `run.quote` and `run.start` from the control
channel, the MCP bridge, the built-in MCP server and the in-canvas agent were refused with
"provider route is not configured". Routing alone would let any program with the token spend the
person's money, so the two land together.

**Routing.** Before an automation client's `run.quote`, `run.changed`, `run.workflow`,
`run.estimate` or `run.start` reaches the session, the window routes the cards it names, as
Generate does (`NodeCraftApp::automation_run_gate`, `Runs::route_automation`). It never opens a
dialog for it. If a provider still has to be set up, the gate says to wait. The control and
built-in MCP pumps then park the call and ask again each frame, within the client's own 30 s
deadline. The agent is answered `busy` and told to send it again. Routing also keeps the shown
canvas's cards, so a run the person is confirming still reaches its providers. Routing to the same
providers again is skipped: every new routing changes every quote token, and a client's
`run.start` must still match the quote it read.

**Consent.** Before a `run.start` from automation that may spend money, the window checks the
person's decision for that channel: `control` (which includes the bridge), `mcp_server` and
`agent`. The first time, it asks in a modal: **Allow once** (one start with exactly that
`accept`: the quote token and what it may spend), **Always allow** (remembered, up to a per-run
ceiling per currency), **Never** (remembered), or **Not now** (this run only). Decisions live in `[automation.run_consent]` in settings and can be
revoked in the Automation panel.

- **Per channel, not per client.** Clients don't authenticate as anything finer than the
  channel's token, so a name a client gives itself would be a claim, not an identity.
- **The ceiling is on what the run accepts, not on what it costs.** `accept.amounts` is the
  engine's cap on what a run may reserve, so comparing it with the ceiling bounds the spend. A
  run accepting a part of unknown price (`partial`, `unpriced_jobs`) is never covered by a
  ceiling and asks each time.
- **Free means accepted at no charge.** A start that accepts zero in every currency and no
  unknown price can't spend money, so it doesn't ask. The engine still checks that the
  accepted amounts cover the quote.
- **The agent's Approve is consent, per call.** The agent tells its toolbox which call the person
  approved as shown (`Toolbox::call_approved`): its impact is still what they saw. That call
  isn't asked about again. Any other agent call is asked like any other channel.
- **Nothing is relaxed.** Quotes, budgets, the accepted ceiling, unknown-price consent and
  idempotency keys are checked by the engine on every start, as before. Consent comes on top.
- **Giving consent is the person's.** `automation.run_consent_set` and
  `automation.run_consent_answer` are human-only. `automation.run_consent_list` is agent-visible
  and read-only. The CLI has no host for them, so all three are `unsupported`. The browser keeps
  decisions only for the session.
- **Waiting never runs late.** A parked call whose client stopped waiting is dropped
  unanswered: it never started, so a retry can't run it twice.
