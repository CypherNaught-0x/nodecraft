## 2026-11-03 · Custom endpoints with no known price: quotes, budgets and retries (#961)

Status: Accepted, 2026-11-03. The owner decided the open question (may a remote endpoint be
marked free? Yes, with a warning: point 3) and approved the record.

A custom endpoint (#960) is a base URL the person sets for an OpenAI-compatible server: a
LiteLLM proxy, vLLM, LM Studio, a gateway. The shipped catalog has no price for the models such a
server lists, so the run engine needs a rule for quoting, admitting, settling and retrying them.
This record settles that rule. Most cases reuse a path that already exists for another
provider, and each is cited. One is new tracking work: free custom-endpoint jobs must still
record what the endpoint reports (point 3).

Scope: models the endpoint serves that the catalog does not price. A catalog model that #960
routes to a custom base URL keeps its catalog identity and a non-zero catalog price. A catalog
step that would be free or zero there is unpriced (#960, `custom_endpoints::apply` in
`crates/engine/src/custom_endpoints.rs` on PR #1029) unless the endpoint is marked free.
Nothing here lowers a non-zero catalog price.

### What the code does today

- **Quotes.** `exec::cost::quote` gives each step `per_job: Result<Option<Money>, Unpriced>`.
  `Ok(None)` is free, `Err(Unpriced::…)` is unknown (`crates/exec/src/cost.rs`). `run.quote`
  shows the reason (`unpriced_reason` in `crates/engine/src/run_cmds.rs`).
- **Budgets.** `cost::check` adds `Block::Unknown` for any unpriced step whenever a per-call,
  session, project or total cap is set. A capped run with an unknown step cannot start. The
  module's own rule is that "a budget can't be passed by a quote that was silently incomplete"
  (`crates/exec/src/cost.rs`, and [the quotes and budgets record](2026-10-17-cost-quotes-and-budgets.md)).
- **Consent.** With no cap, `accept_quote` in `run_cmds.rs` requires `partial: true` and an
  `unpriced_jobs` count that covers the quote. The run confirmation shows "Accept the unknown
  part of the price" and sends both (`crates/ui-egui/src/app/generate.rs`). When a cap blocks the
  run, the only remedy offered is "Change budgets…".
- **Per-run ceiling.** `RunCeiling::admit_unpriced` (`crates/engine/src/run_ceiling.rs`) admits
  one unknown-price job for each new provider and key, up to the accepted count. A retry under
  the same key uses no extra admission. An unknown-price admission is never given back.
- **At job time.** `reserve` in `crates/engine/src/jobs.rs` refuses an unpriced job under any cap
  ("the price of … isn't known, so the budget can't be checked"). Without a cap it records a
  metadata shell (`UsageLog::reserve_metadata`). `accounting::finish` then records the reported
  cost, if there is one, or `NoReportedCost` (`crates/engine/src/jobs/accounting.rs`).
- **The #833 precedent.** Partner nodes are marked at admission by `NodeKind::unpriced`
  (`crates/nodes/src/comfy_workflow.rs`). Legacy cards (#872) and the hosted ComfyUI profile are
  marked by `comfy_partner_quotes::apply` (`crates/engine/src/comfy_partner_quotes.rs`). Both
  give `Unpriced::Invalid(reason)`, and `comfy_partner_quotes::bind` puts the evidence into the
  quote token, so a change refuses `run.start` ([partner nodes](2026-10-23-comfy-partner-nodes-unpriced.md),
  [hosted profile](2026-10-24-hosted-comfy-profile.md): "Finite budgets reject them; uncapped
  runs require the existing explicit partial quote consent").
- **LiteLLM spend.** `reported_cost` in `crates/providers/src/llm/mod.rs` reads
  `x-litellm-response-cost` into `Usage::cost`, through `usd`, which drops NaN, negative and
  absurd values. `chat_response` reads it before `error_for`, so a non-2xx reply's cost is
  discarded with the error. A 2xx reply's `0` is taken as zero. `jobs::measured` prefers
  `Usage::cost` over token rates. The header is read for every `LlmClient` endpoint, whatever its
  id. #489 records that LiteLLM writes `0` on a failure row where the cost is missing or
  unusable ("a gateway reports `0` where the truth is 'unknown'"). It does not establish what a
  2xx `0` means.
- **Idempotency and retries.** Scheduler retries of a transient failure keep the job's key.
  `jobs::sent_key` derives `base#n` only after the provider reports a failed generation, and
  `Scheduler::requeue` (the queue's "retry") gives a new key. Chat APIs take no idempotency key
  (the comment on `impl Provider for LlmClient`). Whether an ambiguous no-handle submit is retried
  is `Provider::retry_ambiguous_submit` (`crates/providers/src/provider.rs`). For `LlmClient` it
  is `true`, except for `azure-openai`. A failure is retried when it is transient and has a
  handle, carries `NotSent` evidence, or the provider allows ambiguous retries (`will_retry` in
  `jobs.rs`). When `retry_ambiguous_submit` is `false`, an **uncertain** failure is terminal, and
  the reservation closes as `Charge::Unresolved` at the estimate (`jobs::close`). `LlmClient` never
  calls `SubmitContext::refused`, so it has no documented-refusal evidence: an HTTP reply of any
  kind does not prove the request was not billed.
- **Free steps are untracked.** `QueuedJob` building in `run_cmds.rs` gives an `Ok(None)` step
  `price = None` ("free work is neither debited nor counted"), so `jobs.rs` reserves no metadata
  shell and `accounting::finish` returns `NotBudgeted`: a cost a free job reports is dropped.
- **Public or local.** `providers::http::public_address` (`crates/providers/src/http/public_only.rs`)
  treats anything that is not public unicast as not public, which includes carrier NAT,
  reserved, documentation, multicast and tunnelled forms. It is an SSRF guard, not a local test.
  #960 adds a positive list for "local", `providers::base_url::BaseUrl::is_local`
  (`crates/providers/src/base_url.rs` on PR #1029), and uses it to allow plain HTTP.
- **Settings and panels.** Endpoint addresses live in `[providers]` settings
  (`ProviderSettings::litellm_url` in `crates/engine/src/settings.rs`). Keys stay in the keychain
  (`crates/ui-egui/src/keys.rs`). Caps are `[budgets]` settings, and "not a command: no agent or
  script can raise them" (`crates/ui-egui/src/budgets.rs`). The project
  cap is `this_project` (#847).

### Decision

**One cost rule per custom endpoint, kept in settings.** Each endpoint gets one rule: **unpriced**
(the default), **priced by you**, or **free**: *free (local)* on a local host, or
*free (remote, unverified)* on any other host after an explicit warning (point 3). The rule is
stored with the endpoint's URL in `[providers]` settings (the browser keeps it with the per-site
server addresses). It is never stored in a project file, a key or the keychain.

- *Only the person loosens it.* Like the budget caps, the rule is the person's setting, not a
  command. No agent, MCP tool, control channel or script can set it, because marking an endpoint
  free or lowering its price loosens every budget. Commands may read it. The engine may change it
  only to **tighten** it (free or priced, to unpriced: points 3 and the URL rule below), never to
  loosen it.
- *Bound into the quote.* The quote binding, alongside `comfy_partner_quotes::bind`, takes the
  endpoint's instance id, its **normalised base URL**, the rule kind and the **exact rates**. Any
  change after a quote refuses `run.start` with "quote again".
- *Tied to the URL.* Changing the base URL's scheme, host or port resets any rule to unpriced.
  A path-only edit keeps the rule, since the host is unchanged. A rule never carries over to a
  host it wasn't chosen for.

**1. What a quote shows, and whether a budget can still be set.**

- *Unpriced* (no rule set). The step is `Err(Unpriced::Invalid("<endpoint> has no price set;
  set one in API keys"))`. This is the same variant #833 uses, so `run.quote`, the run
  confirmation and the CLI or MCP show it with no new code path. The quote is partial, and the
  confirmation keeps its "Accept the unknown part of the price" checkbox and its `unpriced_jobs`
  count.
- *Budgets* can still be set, and they keep their meaning. Under any cap, an unpriced endpoint
  step is blocked (`Block::Unknown`), exactly like a partner node. There is **no per-run
  override under a cap.** #833 has none either, and adding one would break the invariant that a
  cap is never passed by an incomplete quote. Without a cap, the run needs the existing explicit
  per-run consent (`partial` plus `unpriced_jobs`), and the per-run ceiling bounds how many
  unknown-price jobs it may send.
- *Priced by you.* The person enters rates in USD: per 1,000 input tokens, per 1,000 output
  tokens, and optionally a fixed amount per call. Each rate may be zero, but the three together
  may not all be zero (a zero price is the free rule, with its own limits). They become a
  `ModelDescriptor`-shaped price for that endpoint's models. The quote is the per-call amount,
  plus `max_tokens` at the output rate (the shape the shipped chat models use,
  `assets/catalog/anthropic.toml`), **plus an input estimate**: the bytes of every text input
  (system text, the prompt, earlier turns) plus a fixed per-message allowance for the chat
  template, taken as an upper bound on input tokens, at the input rate. The catalog precedent
  quotes output only, so input tokens can push its actual spend past the reservation; the byte
  bound closes that gap for custom endpoints, at the cost of quoting high.
  - Other OpenAI-compatible servers get a reply limit only when the job sets one
    ([LLM endpoints](2026-10-17-llm-endpoints.md)). A priced custom step therefore always sends
    `max_tokens`, the default when the job sets none, so the output bound holds.
  - The per-call amount is not part of `TokenRates` (`crates/catalog/src/pricing.rs`), so
    settlement from measured tokens adds it to the token charge explicitly.
  - Images in the prompt can't be bounded this way; a step with image inputs still quotes the
    text bound, and its actual spend can exceed the reservation, as the catalog's does.
  - The confirmation labels the price "priced by you", so nobody reads it as a provider list
    price. Rates are parsed as exact `Money`, never as floats, and bounded by
    `MAX_CHARGE_MICROS`.
- *Free (local).* The step quotes nothing, like a local ComfyUI workflow, but is still tracked
  (point 3). Free (local) is the person's statement that the server runs the model itself
  (vLLM, LM Studio, Ollama). NodeCraft can't check it: a local proxy that forwards to paid
  upstreams and doesn't report cost would spend outside every cap. Under any cap, the legacy
  `litellm` profile (`ProviderSettings::litellm_url` in `crates/engine/src/settings.rs`), and
  any endpoint that has ever sent `x-litellm-response-cost`, counts as **unpriced even when
  marked free**, so it is blocked like #833. A LiteLLM proxy set as another provider's base URL
  (#960) carries no LiteLLM marker until it reports, so the API keys panel says this next to
  the choice.
- *Free (remote, unverified).* Quoted and tracked like free (local), but **under any cap it
  counts as unpriced** and is blocked (`Block::Unknown`), like #833. Without a cap it runs as
  free. The rules for setting it are in point 3.

**2. How a retry is charged.** A custom endpoint is assumed **not** to honour idempotency keys:
LiteLLM drops `Idempotency-Key` (#489), and vLLM and LM Studio document none. The policy
follows the server, not the model: **every `LlmClient` with a user-set base URL**, including
`litellm` and catalog-priced models #960 routes there, returns `retry_ambiguous_submit() ==
false`, as Azure does today and as the
[Gemini compatible endpoint](2026-10-24-gemini-text-compatible-endpoint.md) decided ("No
idempotency guarantee is assumed: an ambiguous paid submit is terminal"). #960 does this for
catalog providers given a base URL. Until the follow-up lands, the `litellm` profile still
resends ambiguous submits and reads the spend header unconditionally, with a `0` read as zero
(#960, "Not done"), so its double-charge exposure remains.

- Only `NotSent` evidence from the transport allows a same-key retry. Today that is a failure
  before the request is handed to the HTTP client: a request that could not be built, a refused
  address literal under `public_only`, or a staging failure. `ReqwestHttp::send_once` marks the
  attempt possibly sent before it calls `execute` ("Delivery becomes possible when execute is
  INVOKED"), so reqwest connect, DNS and TLS errors count as possibly sent, and the browser
  transport marks every attempt the same way (`crates/providers/src/http.rs`). Widening that
  evidence is a separate change. `LlmClient` has no documented-refusal list (it never calls
  `SubmitContext::refused`), so **any HTTP reply, including a 4xx or 5xx, counts as possibly
  billed.** Releasing a job credits the ceiling back only on `NotSent` evidence (`jobs::close`).
- Anything else (a timeout after sending, a dropped stream, a garbled reply, any error status) is
  terminal. That includes a 429 with `retry-after`, which LM Studio and vLLM send under load:
  the person retries by hand, under a new key. This is deliberate. A priced job is charged
  `Unresolved` at its estimate and stays in the usage log, where the ownership and quarantine
  tools can reconcile it. An unpriced job keeps its
  unknown-price admission, which is never refunded (`run_ceiling.rs`). No cost is recorded for
  it, since an ambiguous failure has no 2xx reply to read a report from.
- A retry the person asks for (`Scheduler::requeue`) is a new job under a new key. It needs a new
  reservation, or a new unknown-price admission, within the run's accepted ceiling. A retry is
  never free, and it is never folded into the earlier attempt's charge.

**3. Whether a local address counts as free, and how local is told from remote.** No address
is **ever free by default.** A LiteLLM proxy on `127.0.0.1` can forward to paid
upstreams, so "local" says nothing about cost. An endpoint counts as free only when the person
marks it free. *Free (local)* is offered when the base URL's host is local:

- a host for which `providers::base_url::BaseUrl::is_local` is true (#960,
  `crates/providers/src/base_url.rs` on PR #1029): loopback, RFC 1918, link-local, unique local
  IPv6, the IPv4-mapped forms of the IPv4 ones, or exactly `localhost`.
- Carrier NAT (`100.64.0.0/10`, which includes Tailscale peers), Teredo, 6to4, NAT64, the 6to4
  relay range, reserved `240.0.0.0/4`, multicast, unspecified, benchmarking and documentation
  ranges are remote.

This is the same positive list #960 uses to allow plain HTTP. It is narrower than the SSRF
check `providers::http::public_address` ("not public"), which is not used here. Any other host
name counts as remote, even if it resolves to a private address today. Its records can change
after the choice was made, and the browser build cannot resolve names at all. Names under
`.localhost` are not accepted either: some resolvers send them to DNS. A person with a LAN host
name uses its IP address to mark it free (local), or marks the name free (remote, unverified).

*A remote endpoint may be marked free, with a warning* (the owner's decision, 2026-11-03). For
a self-hosted GPU box on a public address, a small non-zero price is no longer the only way.
The rule for a host that is not on the `BaseUrl::is_local` list:

- *Who and where.* Only the person, in settings (the API keys panel), can mark it free. No
  command, agent, MCP tool, control channel or script can, as for every loosening above.
- *An explicit warning first.* Choosing it opens a confirmation that must be accepted. The
  warning says that NodeCraft can't verify that the endpoint is free; that runs on it will not
  count against budgets (so while any budget is set, they are blocked, below); and that the key
  and the data of every run are sent to that remote host. Dismissing it leaves the rule as it
  was.
- *Bound to the host.* The acknowledgement is stored with the rule and records the scheme, host
  and port it was given for. Changing any of them resets the endpoint to unpriced (the URL rule
  above), and a remote free mark with no acknowledgement for the endpoint's current scheme, host
  and port reads as unpriced. A path-only edit keeps it.
- *The local safeguards still apply.* The legacy `litellm` profile, and any endpoint that has
  ever sent `x-litellm-response-cost`, counts as unpriced under a cap even when marked free. A
  non-zero reported cost withdraws the mark (below), back to unpriced; re-marking it free means
  accepting the warning again. Free jobs take zero reservations (below).
- *Under a cap it counts as unpriced.* A remote free endpoint is blocked under any per-call,
  session, project or total cap, exactly like an unpriced step, and there is no per-run override.
  This is the conservative choice and the one consistent with "runs cost money": a budget is the
  person's promise that spend stays under a number, and an unverified claim that a remote host
  costs nothing can't keep it. The other option, a per-run consent under a cap, is the override
  this record rejects (Consequences), and a remote endpoint is no reason to add it for one
  provider alone. So a remote free mark changes only uncapped runs: they quote nothing and need
  no `partial` consent, where an unpriced endpoint needs it each run. The person already gave
  that consent once, in the warning, for this host.

*Free jobs are still tracked.* Today a free step gets `price = None` and leaves no usage row
(see above), so a reported cost would be lost. A free custom-endpoint step instead carries a
`JobPrice` with a zero estimate. That is a reservation, not a metadata shell: `jobs::reserve`
takes its priced path, checks the caps, records a zero `Estimate` row and makes a zero ceiling
debit, and `accounting::finish` settles it at whatever the endpoint reports. So free jobs count
toward `MAX_CEILING_RESERVATIONS` (`crates/engine/src/run_ceiling.rs`) and get the host-setup
refusals, such as a budget with no session id. This new work for #960 is a deliberate exception
to [budgets at job time](2026-10-17-budgets-at-job-time.md) ("jobs without a `JobPrice` skip the
budget"). It also needs two changes to #960 as built: `custom_endpoints::apply` must leave free
(local or remote) steps free instead of making them unpriced, and `LlmClient::reported_cost`,
which returns `None` on every custom base URL, must be read for free endpoints.

*A report withdraws the mark.* When a free endpoint reports a non-zero cost:

- that cost is recorded as spend;
- the endpoint's rule is tightened to *unpriced*, and the person is told why (re-marking it free
  is their own decision, in settings);
- every run's **unsubmitted** jobs on that endpoint are refused at reservation with a budget
  refusal, since their quote no longer holds. The rule is re-read in `reserve`, not only at
  `run.start`, so runs already authorized on the endpoint stop too;
- the tightened rule is written so that a later settings save from a stale copy (the UI's)
  can't restore *free*. Only an explicit choice in settings loosens it again.

Spend already in flight when the report arrives (jobs submitted in parallel, or a multi-request
job) is not stopped and **can exceed a cap**. This is the same limit any cost discovered after
dispatch has; the rules above keep it to one batch of in-flight work on that endpoint **when
the endpoint reports its cost**. An endpoint that never reports is not bounded by any cap: that
is what marking it free means.

**4. How the rule appears in the API keys panel and in project budgets.**

- *API keys panel.* Each custom endpoint's row shows its URL, a local or remote badge from the
  classification above, and a **Cost** choice: *Unpriced* (the default), *Priced by me* (three
  amount fields), *Free (local)* for a local host, or, for a remote host, *Free (remote,
  unverified)*, which opens the warning (point 3) before it applies. A remote endpoint marked
  free is shown as "free (remote, unverified)", never as plain "free". The panel says what each
  choice means for budgets. For example, unpriced says "Runs are blocked while any budget is set,
  and need your consent each time otherwise". Free (local) says that NodeCraft can't check it,
  and that an endpoint that forwards to paid upstreams without reporting cost would spend
  outside every cap. LiteLLM's row says that free (local) still blocks under a budget, and
  offers **Trust LiteLLM's reported cost** (off by default; point 5). The rule is saved to
  settings with the endpoint, never to the keychain, and the key field is unchanged.
- *Budgets panel and project budgets.* The caps are unchanged: per call, session, this project
  (#847), and total, in USD. When a cap is set and the person has any custom endpoint that would
  be blocked (unpriced, or a free endpoint that counts as unpriced under a cap, point 1), the
  Budgets panel lists it, a remote free one labelled "free (remote, unverified)": "Runs on these
  are blocked while a cap is set: set a price in API keys". When a capped run is blocked by such
  an endpoint, the run confirmation offers **Set a price…** (it opens that endpoint's API keys
  row) next to "Change budgets…". Spend on a custom endpoint counts toward the project cap through
  the usage log's `project` and `instance` fields, like any other provider.

**5. How options 1–4 from the issue combine.** For a model on a custom endpoint, in this order:

1. A **non-zero catalog price**, when the catalog has the model: unchanged by #960. A catalog
   step that would be free or zero is unpriced on a custom base URL (`custom_endpoints::apply`)
   unless the endpoint is marked free (local or remote).
2. Otherwise the endpoint's **rule**: free (local), only on a local host the person marked, and
   never under a cap for the legacy `litellm` profile or an endpoint that has sent
   `x-litellm-response-cost`; free (remote, unverified), only on a remote host the person marked
   after the warning, and never under any cap; priced by you, at the entered rates; or unpriced,
   the default.

The **endpoint's own report** (option 1) is a *settlement* input only. It never prices a quote,
never admits a run under a cap, and never frees room in the run's ceiling. A dearer report is
recorded as it is, and a cheaper one doesn't reopen room (`run_ceiling.rs`, "Releases"). It
also never lowers a priced job's settled charge below its estimate, so it can't free room in a
session, total or project cap either: those caps are checked against the usage log.

- *Whose choice.* Trusting the report is the person's choice, made per LiteLLM endpoint in API
  keys. `reported_cost` must stop reading the header from other endpoints, since any server can
  send it. Free endpoints are the exception: any non-zero report is recorded and withdraws the
  mark (point 3), because there the report can only tighten.
- *Zero.* A reported `0` counts as **no report**. This is the conservative choice: #489 records
  that LiteLLM writes `0` where the cost is unknown on a failure row, and a 2xx `0` can't be told
  apart from "no price known".

So:

- Unpriced: settled at the trusted report if there is one, else no cost is recorded (as today).
  The job counts against `unpriced_jobs`.
- Priced by you, and catalog-priced models on a custom base URL (#960): settled at the
  **larger** of the estimate and the measured charge (the trusted report, else the measured
  tokens at the rates, plus the per-call amount). The endpoint's numbers can raise the charge,
  never lower it below the estimate (`jobs::settled` in `crates/engine/src/jobs.rs` on PR
  #1029). The reason: a custom server's token counts and reports can't be checked. An endpoint
  that under-reports (`0` or `1` tokens) would otherwise settle below its reservation, the usage
  log would show less spend than was made, and the session, total and project caps would admit
  more later runs than the person's own rates justify.
- Free (local) and free (remote, unverified): cost nothing, unless a non-zero report withdraws
  the mark (point 3).

Option 4 ("free by default for local") is rejected as a default and kept only as an explicit
choice: unwarned for a local host, behind a warning and never under a cap for a remote one.
Option 3 is the default. Option 2 is the remedy that makes a custom endpoint usable under a
budget.

### Consequences and work for #960

- `LlmClient`: every endpoint with a user-set base URL returns `retry_ambiguous_submit() ==
  false` (#960 does this for catalog providers given a base URL; the `litellm` profile follows),
  and only failures carrying transport `NotSent` evidence are retried. `reported_cost` is read
  only from a LiteLLM endpoint whose report the person trusts, or from a free endpoint; a 2xx
  `0` becomes `None`. Fixture tests cover a `0` header, a header from an untrusted endpoint, a
  5xx reply that must not be resent, and an ambiguous submit that must not be resent.
- **New tracking work:** free custom-endpoint steps carry a zero-estimate `JobPrice`, reserved
  as a zero `Estimate` row (point 3), so their reported cost reaches the usage log. A non-zero
  report tightens the rule to unpriced and refuses every run's unsubmitted jobs on that endpoint
  at reservation.
- An engine pass, #960's `custom_endpoints::apply`, run after `provider_quotes::apply` in both
  `run.quote` and `run.start` (`crates/engine/src/run_cmds.rs` on PR #1029), sets
  `Unpriced::Invalid` (never `ByUsage`, so no provider quote re-prices it) or the user-entered
  price (with the input estimate) for each custom-endpoint step, and treats a free endpoint that
  counts as unpriced under a cap (point 1) as unpriced. Its binding (instance, normalised URL,
  rule kind, rates) goes into the quote token. Golden tests run against the mock provider:
  blocked under each cap; uncapped with and without consent; a rule or URL changed after a
  quote; a free endpoint reporting a cost mid-run; a requeue under a new key using a new
  admission.
- Settings gain an optional cost rule per endpoint. A missing or unreadable rule reads as
  *unpriced*, never as free, as an unreadable cap reads as `0` (`settings/budgets.rs`). Engine
  writes to the rule may only tighten it, and a save from a stale copy can't undo a tightening.
- The UI work is in `keys.rs`, `budgets.rs` and the run confirmation in `generate.rs`.
- Rejected: a per-run "accept the risk" override under a cap. It is the one choice here that
  would let an unknown amount pass a budget. If the owner wants one, it belongs in a separate
  decision that also covers partner nodes and hosted ComfyUI, so all unknown-price runs keep one
  rule.

- Settings store the remote warning's acknowledgement with the rule, keyed to the scheme, host
  and port it was given for (point 3). The confirmation dialog is in `keys.rs`.
- Not in #1029: PR #1029 implements no free marking at all, local or remote, so this decision
  needs no code change there. Free (local), free (remote, unverified) and their tracking are
  follow-up work.

The owner was asked whether a remote endpoint (a self-hosted GPU box on a public address) may
ever be marked free, and answered on 2026-11-03: yes, with a warning (point 3).
