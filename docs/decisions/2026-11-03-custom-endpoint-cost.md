## 2026-11-03 · Custom endpoints with no known price: quotes, budgets and retries (#961)

Status: Proposed: needs the owner's agreement before #960's paid runs use it.

A custom endpoint (#960) is a base URL the person sets for an OpenAI-compatible server: a
LiteLLM proxy, vLLM, LM Studio, a gateway. The shipped catalog has no price for the models such a
server lists, so the run engine needs a rule for quoting, admitting, settling and retrying them.
This record settles that rule. Most cases reuse a path that already exists for another
provider, and each is cited. One is new tracking work: free custom-endpoint jobs must still
record what the endpoint reports (point 3).

Scope: models the endpoint serves that the catalog does not price. A catalog model that #960
routes to a custom base URL keeps its catalog identity and price (#960, "without changing the
model's identity or cost rules"); nothing here lowers or removes a catalog price.

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
- **The #833 precedent.** ComfyUI partner nodes and the hosted ComfyUI profile follow exactly
  this path. `comfy_partner_quotes::apply` turns their steps into `Unpriced::Invalid(reason)`,
  and `comfy_partner_quotes::bind` puts the evidence into the quote token, so a change refuses
  `run.start` ([partner nodes](2026-10-23-comfy-partner-nodes-unpriced.md),
  [hosted profile](2026-10-24-hosted-comfy-profile.md): "Finite budgets reject them; uncapped
  runs require the existing explicit partial quote consent").
- **LiteLLM spend.** `reported_cost` in `crates/providers/src/llm/mod.rs` reads
  `x-litellm-response-cost` into `Usage::cost`, through `usd`, which drops NaN, negative and
  absurd values. `chat_response` reads it before `error_for`, so a non-2xx reply's cost is
  discarded with the error. A 2xx reply's `0` is taken as zero. `jobs::measured` prefers
  `Usage::cost` over token rates. The header is read for every `LlmClient` endpoint, whatever its
  id. LiteLLM itself reports `0` when it has no price for a model (#489, "a gateway reports `0`
  where the truth is 'unknown'").
- **Idempotency and retries.** Scheduler retries of a transient failure keep the job's key.
  `jobs::sent_key` derives `base#n` only after the provider reports a failed generation, and
  `Scheduler::requeue` (the queue's "retry") gives a new key. Chat APIs take no idempotency key
  (the comment on `impl Provider for LlmClient`). Whether an ambiguous no-handle submit is retried
  is `Provider::retry_ambiguous_submit` (`crates/providers/src/provider.rs`). For `LlmClient` it
  is `true`, except for `azure-openai`. When it is `false`, the failure is terminal, and the
  reservation closes as `Charge::Unresolved` at the estimate (`jobs::close`). `LlmClient` never
  calls `SubmitContext::refused`, so it has no documented-refusal evidence: an HTTP reply of any
  kind does not prove the request was not billed.
- **Free steps are untracked.** `QueuedJob` building in `run_cmds.rs` gives an `Ok(None)` step
  `price = None` ("free work is neither debited nor counted"), so `jobs.rs` reserves no metadata
  shell and `accounting::finish` returns `NotBudgeted`: a cost a free job reports is dropped.
- **Public or local.** `providers::http::public_address` (`crates/providers/src/http/public_only.rs`)
  classifies an IP as public unicast. Loopback, RFC 1918, carrier NAT, link-local and
  metadata, IPv6 ULA and link-local, and mapped or 6to4 forms of all of these count as not
  public. `ReqwestHttp::public_only` uses it on IP literals, and its resolver uses it on resolved
  names. That module notes that a name's DNS records "can point anywhere".
- **Settings and panels.** Endpoint addresses live in `[providers]` settings
  (`ProviderSettings::litellm_url` in `crates/engine/src/settings.rs`). Keys stay in the keychain
  (`crates/ui-egui/src/keys.rs`). Caps are `[budgets]` settings, and "not a command: no agent or
  script can raise them" (`crates/ui-egui/src/budgets.rs`). The project
  cap is `this_project` (#847).

### Decision

**One cost rule per custom endpoint, kept in settings.** Each endpoint gets one rule: **unpriced**
(the default), **priced by you**, or **free (local)**. The rule is stored with the endpoint's URL
in `[providers]` settings (the browser keeps it with the per-site server addresses). It is never
stored in a project file, a key or the keychain.

- *Only the person loosens it.* Like the budget caps, the rule is the person's setting, not a
  command. No agent, MCP tool, control channel or script can set it, because marking an endpoint
  free or lowering its price loosens every budget. Commands may read it. The engine may change it
  only to **tighten** it (free or priced, to unpriced: points 3 and the URL rule below), never to
  loosen it.
- *Bound into the quote.* The quote binding, alongside `comfy_partner_quotes::bind`, takes the
  endpoint's instance id, its **normalised base URL**, the rule kind and the **exact rates**. Any
  change after a quote refuses `run.start` with "quote again".
- *Tied to the URL.* Editing the base URL keeps a priced rule but resets a free rule to unpriced
  unless the new host is also local (point 3). A rule never carries over to a host it wasn't
  chosen for.

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
  `assets/catalog/anthropic.toml`), **plus an input estimate**: the prompt's length in bytes,
  taken as an upper bound on its tokens, at the input rate. The catalog precedent quotes output
  only, so input tokens can push its actual spend past the reservation; the byte bound closes
  that gap for custom endpoints, at the cost of quoting high. Images in the prompt can't be
  bounded this way; a step with image inputs still quotes the text bound, and its actual spend
  can exceed the reservation, as the catalog's does. The confirmation labels the price "priced
  by you", so nobody reads it as a provider list price. Rates are parsed as exact `Money`, never
  as floats, and bounded by `MAX_CHARGE_MICROS`.
- *Free (local).* The step quotes nothing, like a local ComfyUI workflow, but is still tracked
  (point 3). Under any cap, an endpoint configured as **LiteLLM** (a proxy that can forward to
  paid upstreams and reports cost) counts as **unpriced even when marked free**, so it is
  blocked like #833. Free (local) under a cap is for servers that run the model themselves
  (vLLM, LM Studio, Ollama).

**2. How a retry is charged.** A custom endpoint is assumed **not** to honour idempotency keys:
LiteLLM drops `Idempotency-Key` (#489), and vLLM and LM Studio document none. The policy
follows the server, not the model: **every `LlmClient` with a user-set base URL**, including
`litellm` and catalog-priced models #960 routes there, returns `retry_ambiguous_submit() ==
false`, as Azure does today and as the
[Gemini compatible endpoint](2026-10-24-gemini-text-compatible-endpoint.md) decided ("No
idempotency guarantee is assumed: an ambiguous paid submit is terminal").

- Only **transport evidence that nothing was sent** allows a same-key retry: the connection was
  refused, or name resolution or the TLS handshake failed before the body went out. `LlmClient`
  has no documented-refusal list (it never calls `SubmitContext::refused`), so **any HTTP reply,
  including a 4xx or 5xx, counts as possibly billed.** Releasing a job credits the ceiling back
  only on that evidence (`jobs::close`).
- Anything else (a timeout after sending, a dropped stream, a garbled reply, any error status) is
  terminal. A priced job is charged `Unresolved` at its estimate and stays in the usage log,
  where the ownership and quarantine tools can reconcile it. An unpriced job keeps its
  unknown-price admission, which is never refunded (`run_ceiling.rs`). No cost is recorded for
  it, since an ambiguous failure has no 2xx reply to read a report from.
- A retry the person asks for (`Scheduler::requeue`) is a new job under a new key. It needs a new
  reservation, or a new unknown-price admission, within the run's accepted ceiling. A retry is
  never free, and it is never folded into the earlier attempt's charge.

**3. Whether a local address counts as free, and how local is told from remote.** A local
address is **never free by default.** A LiteLLM proxy on `127.0.0.1` can forward to paid
upstreams, so "local" says nothing about cost. An endpoint counts as free only when the person
picks *free (local)*, and that choice is offered only when the base URL's host is local:

- an IP literal for which `providers::http::public_address` is false (loopback, private,
  link-local, carrier NAT, ULA, and mapped forms);
- or exactly `localhost`.

Any other host name counts as remote, even if it resolves to a private address today. Its
records can change after the choice was made (`public_only.rs`), and the browser build cannot
resolve names at all. Names under `.localhost` are not accepted either: some resolvers send them
to DNS. A person with a LAN host name uses its IP address to mark it free. The same
classification serves #960's rule that plain HTTP is allowed only for a local or private
address. A remote endpoint can be unpriced or priced by you, but never free.

*Free jobs are still tracked.* Today a free step gets `price = None` and leaves no usage row
(see above), so a reported cost would be lost. A free custom-endpoint step instead carries a
zero-estimate `JobPrice` (or an equivalent metadata marker), so `jobs.rs` reserves a metadata
shell and `accounting::finish` records whatever the endpoint reports. This is new work for #960.

*A report withdraws the mark.* When a free endpoint reports a non-zero cost:

- that cost is recorded as spend;
- the endpoint's rule is tightened to *unpriced*, and the person is told why (re-marking it free
  is their own decision, in settings);
- the run's remaining **unsubmitted** jobs on that endpoint fail with a budget refusal before
  they are sent, since their quote no longer holds.

Spend already in flight when the report arrives (jobs submitted in parallel, or a multi-request
job) is not stopped and **can exceed a cap**. This is the same limit any cost discovered after
dispatch has; the rules above keep it to one batch of in-flight work on that endpoint.

**4. How the rule appears in the API keys panel and in project budgets.**

- *API keys panel.* Each custom endpoint's row shows its URL, a local or remote badge from the
  classification above, and a **Cost** choice: *Unpriced* (the default), *Priced by me* (three
  amount fields), or *Free (local)*, which is hidden for remote hosts. The panel says what each
  choice means for budgets. For example, unpriced says "Runs are blocked while any budget is set,
  and need your consent each time otherwise". LiteLLM's row says that free (local) still blocks
  under a budget, and offers **Trust LiteLLM's reported cost** (off by default; point 5). The
  rule is saved to settings with the endpoint, never to the keychain, and the key field is
  unchanged.
- *Budgets panel and project budgets.* The caps are unchanged: per call, session, this project
  (#847), and total, in USD. When a cap is set and the person has any custom endpoint that would
  be blocked (unpriced, or a free LiteLLM endpoint), the Budgets panel lists it: "Runs on these
  are blocked while a cap is set: set a price in API keys". When a capped run is blocked by such
  an endpoint, the run confirmation offers **Set a price…** (it opens that endpoint's API keys
  row) next to "Change budgets…". Spend on a custom endpoint counts toward the project cap through
  the usage log's `project` and `instance` fields, like any other provider.

**5. How options 1–4 from the issue combine.** For a model on a custom endpoint, in this order:

1. A **catalog price**, when the catalog has the model: unchanged by #960.
2. Otherwise the endpoint's **rule**: free (local), only on a local host the person marked, and
   never under a cap for a LiteLLM endpoint; priced by you, at the entered rates; or unpriced,
   the default.

The **endpoint's own report** (option 1) is a *settlement* input only. It never prices a quote,
never admits a run under a cap, and never frees room in the run's ceiling. A dearer report is
recorded as it is, and a cheaper one doesn't reopen room (`run_ceiling.rs`, "Releases").

- *Whose choice.* Trusting the report is the person's choice, made per LiteLLM endpoint in API
  keys. `reported_cost` must stop reading the header from other endpoints, since any server can
  send it. Free endpoints are the exception: any non-zero report is recorded and withdraws the
  mark (point 3), because there the report can only tighten.
- *Zero.* A reported `0` counts as **no report**, because LiteLLM writes `0` for "no price
  known".

So:

- Unpriced: settled at the trusted report if there is one, else no cost is recorded (as today).
  The job counts against `unpriced_jobs`.
- Priced by you: settled at the **larger** of the trusted report and the measured tokens at your
  rates, else at the estimate. A report can raise the charge, never lower it below your rates.
- Free (local): costs nothing, unless a non-zero report withdraws the mark (point 3).

Option 4 ("free by default for local") is rejected as a default and kept only as an explicit,
local-only choice. Option 3 is the default. Option 2 is the remedy that makes a custom endpoint
usable under a budget.

### Consequences and work for #960

- `LlmClient`: every endpoint with a user-set base URL returns `retry_ambiguous_submit() ==
  false`, and only pre-send transport failures are retried. `reported_cost` is read only from a
  LiteLLM endpoint whose report the person trusts, or from a free endpoint; a 2xx `0` becomes
  `None`. Fixture tests cover a `0` header, a header from an untrusted endpoint, a 5xx reply that
  must not be resent, and an ambiguous submit that must not be resent.
- **New tracking work:** free custom-endpoint steps carry a zero-estimate `JobPrice` (or a
  metadata marker), so their reported cost reaches the usage log. A non-zero report tightens
  the rule to unpriced and fails the run's unsubmitted jobs on that endpoint.
- An engine pass beside `comfy_partner_quotes::apply` sets the unpriced reason or the
  user-entered price (with the input estimate) for each custom-endpoint step, and treats a free
  LiteLLM endpoint as unpriced under a cap. Its binding (instance, normalised URL, rule kind,
  rates) goes into the quote token. Golden tests run against the mock provider: blocked under
  each cap; uncapped with and without consent; a rule or URL changed after a quote; a free
  endpoint reporting a cost mid-run; a requeue under a new key using a new admission.
- Settings gain an optional cost rule per endpoint. A missing or unreadable rule reads as
  *unpriced*, never as free, as an unreadable cap reads as `0` (`settings/budgets.rs`). Engine
  writes to the rule may only tighten it.
- The UI work is in `keys.rs`, `budgets.rs` and the run confirmation in `generate.rs`.
- Rejected: a per-run "accept the risk" override under a cap. It is the one choice here that
  would let an unknown amount pass a budget. If the owner wants one, it belongs in a separate
  decision that also covers partner nodes and hosted ComfyUI, so all unknown-price runs keep one
  rule.

Open for the owner: whether a remote endpoint (a self-hosted GPU box on a public address) may
ever be marked free. This record says no; the person enters a small non-zero price instead.
