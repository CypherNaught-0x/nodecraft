## 2026-11-03 · Custom endpoints with no known price: quotes, budgets and retries (#961)

Status: Proposed: needs the owner's agreement before #960's paid runs use it.

A custom endpoint (#960) is a base URL the person sets for an OpenAI-compatible server: a
LiteLLM proxy, vLLM, LM Studio, a gateway. The shipped catalog has no price for the models such a
server lists, so the run engine needs a rule for quoting, admitting, settling and retrying them.
This record settles that rule. It adds no new money path: every case below reuses a path that
already exists for another provider, and each is cited.

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
  reservation closes as `Charge::Unresolved` at the estimate (`jobs::close`).
- **Public or local.** `providers::http::public_address` (`crates/providers/src/http/public_only.rs`)
  classifies an IP as public unicast. Loopback, RFC 1918, carrier NAT, link-local and
  metadata, IPv6 ULA and link-local, and mapped or 6to4 forms of all of these count as not
  public. `ReqwestHttp::public_only` uses it on IP literals, and its resolver uses it on resolved
  names. That module notes that a name's DNS records "can point anywhere".
- **Settings and panels.** Endpoint addresses live in `[providers]` settings
  (`ProviderSettings::litellm_url` in `crates/engine/src/settings.rs`). Keys stay in the keychain
  (`crates/ui-egui/src/keys.rs`). Caps are `[budgets]` settings, and "not a command: no agent or
  script can raise them" (`crates/ui-egui/src/budgets.rs`, `settings/budgets.rs`). The project
  cap is `this_project` (#847).

### Decision

**One cost rule per custom endpoint, kept in settings.** Each endpoint gets one rule: **unpriced**
(the default), **priced by you**, or **free (local)**. The rule is stored with the endpoint's URL
in `[providers]` settings (the browser keeps it with the per-site server addresses). It is never
stored in a project file, a key or the keychain. Like the budget caps, the rule is the person's
setting, not a command. No agent, MCP tool, control channel or script can set it, because
marking an endpoint free or lowering its price loosens every budget. Commands may read it. The
rule and the endpoint's instance id go into the quote binding, alongside
`comfy_partner_quotes::bind`, so a rule changed after a quote refuses `run.start` with "quote
again".

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
  tokens, and optionally a fixed amount per call. They become a `ModelDescriptor`-shaped price
  for that endpoint's models. The quote uses `per_unit` / `thousand_tokens` with `count =
  "max_tokens"` at the output rate, plus any per-call amount. This is the shape the shipped chat
  models use (`assets/catalog/anthropic.toml`). The run settles at `TokenRates::cost` of the
  measured usage (`jobs::measured`). The confirmation labels such a price "priced by you", so
  nobody reads it as a provider list price. Rates are parsed as exact `Money`, never as floats,
  and bounded by `MAX_CHARGE_MICROS`. A zero rate is allowed only for a free (local) endpoint,
  below.
- *Free (local).* The step is `Ok(None)` and costs nothing in any cap, like a local ComfyUI
  workflow.

**2. How a retry is charged.** A custom endpoint is assumed **not** to honour idempotency keys:
LiteLLM drops `Idempotency-Key` (#489), and vLLM and LM Studio document none. So the custom
endpoint's `LlmClient` returns `retry_ambiguous_submit() == false`, as Azure does today and as
the [Gemini compatible endpoint](2026-10-24-gemini-text-compatible-endpoint.md) decided ("No
idempotency guarantee is assumed: an ambiguous paid submit is terminal").

- A failure with evidence that nothing was sent (a connection refused, a documented refusal) may
  still be retried under the same key. Releasing such a job credits the ceiling back only when the
  delivery evidence proves non-acceptance (`jobs::close`, `SubmitContext`).
- An ambiguous submit (a timeout after sending, a dropped stream, a garbled reply) is terminal. A
  priced job is charged `Unresolved` at its estimate and stays in the usage log, where the
  ownership and quarantine tools can reconcile it. An unpriced job keeps its unknown-price
  admission, which is never refunded (`run_ceiling.rs`), and any cost the endpoint reported on
  the attempt stays recorded.
- A retry the person asks for (`Scheduler::requeue`) is a new job under a new key. It needs a new
  reservation, or a new unknown-price admission, within the run's accepted ceiling. A retry is
  never free, and it is never folded into the earlier attempt's charge.

**3. Whether a local address counts as free, and how local is told from remote.** A local
address is **never free by default.** A LiteLLM proxy on `127.0.0.1` can forward to paid
upstreams, so "local" says nothing about cost. An endpoint counts as free only when the person
picks *free (local)*, and that choice is offered only when the base URL's host is local:

- an IP literal for which `providers::http::public_address` is false (loopback, private,
  link-local, carrier NAT, ULA, and mapped forms);
- or `localhost`, or a name under `.localhost`.

Any other host name counts as remote, even if it resolves to a private address today. Its
records can change after the choice was made (`public_only.rs`), and the browser build cannot
resolve names at all. A person with a LAN host name uses its IP address to mark it free. The
same classification serves #960's rule that plain HTTP is allowed only for a local or private
address. A remote endpoint can be unpriced or priced by you with non-zero rates, but never free
or zero-priced.

The free mark is withdrawn by evidence. Jobs on a free endpoint are still budget-tracked with a
metadata shell, as unpriced ones are today. When such a job reports a non-zero cost, that cost is
recorded as spend, the endpoint's rule goes back to *unpriced*, and the person is told why.
Re-marking the endpoint free is their own decision, made in settings.

**4. How the rule appears in the API keys panel and in project budgets.**

- *API keys panel.* Each custom endpoint's row shows its URL, a local or remote badge from the
  classification above, and a **Cost** choice: *Unpriced* (the default), *Priced by me* (three
  amount fields), or *Free (local)*, which is hidden for remote hosts. The panel says what each
  choice means for budgets. For example, unpriced says "Runs are blocked while any budget is set,
  and need your consent each time otherwise", and LiteLLM's row adds "LiteLLM's reported cost is
  recorded after each run; it doesn't price the run beforehand". The rule is saved to settings
  with the endpoint, never to the keychain, and the key field is unchanged.
- *Budgets panel and project budgets.* The caps are unchanged: per call, session, this project
  (#847), and total, in USD. When a cap is set and the person has any unpriced custom endpoint,
  the Budgets panel lists those endpoints: "Runs on these are blocked while a cap is set: set a
  price in API keys". When a capped run is blocked by an unpriced endpoint, the run confirmation
  offers **Set a price…** (it opens that endpoint's API keys row) next to "Change budgets…".
  Spend on a custom endpoint counts toward the project cap through the usage log's `project` and
  `instance` fields, like any other provider.

**5. How options 1–4 from the issue combine.** For a model on a custom endpoint, in this order:

1. A **catalog price**, when the catalog has the model: unchanged by #960.
2. Otherwise the endpoint's **rule**: free (local), only on a local host the person marked;
   priced by you, at the entered rates; or unpriced, the default.

The **endpoint's own report** (option 1) is a *settlement* input only. It never prices a quote,
never admits a run under a cap, and never frees room in the run's ceiling. A dearer report is
recorded as it is, and a cheaper one doesn't reopen room (`run_ceiling.rs`, "Releases"). It is
trusted only from an endpoint configured as LiteLLM: `reported_cost` must stop reading the header
from other endpoints, since any server can send it. A reported `0` is treated as **no report**
unless the endpoint is marked free, because LiteLLM writes `0` for "no price known". Settlement
then falls back to measured tokens at the entered rates, or to the estimate. So:

- Unpriced: settled at the LiteLLM report if there is one, else no cost is recorded (as today).
  The job counts against `unpriced_jobs`.
- Priced by you: settled at the LiteLLM report, else at the measured tokens at your rates, else
  at the estimate.
- Free (local): costs nothing, unless a non-zero report withdraws the mark (point 3).

Option 4 ("free by default for local") is rejected as a default and kept only as an explicit,
local-only choice. Option 3 is the default. Option 2 is the remedy that makes a custom endpoint
usable under a budget.

### Consequences and work for #960

- `LlmClient` for a custom endpoint: `retry_ambiguous_submit` returns `false`. `reported_cost`
  is gated to LiteLLM endpoints, and a 2xx `0` becomes `None` unless the endpoint is free.
  Fixture tests cover a `0` header, a header from a non-LiteLLM endpoint, and an ambiguous submit
  that must not be resent.
- An engine pass beside `comfy_partner_quotes::apply` sets the unpriced reason or the
  user-entered price for each custom endpoint step, and its binding goes into the quote token.
  Golden tests run against the mock provider: blocked under each cap; uncapped with and without
  consent; a rule changed after a quote; a requeue under a new key using a new admission.
- Settings gain an optional cost rule per endpoint. A missing or unreadable rule reads as
  *unpriced*, never as free, matching how an unreadable cap reads as `0` in `settings/budgets.rs`.
- The UI work is in `keys.rs`, `budgets.rs` and the run confirmation in `generate.rs`.
- Rejected: a per-run "accept the risk" override under a cap. It is the one choice here that
  would let an unknown amount pass a budget. If the owner wants one, it belongs in a separate
  decision that also covers partner nodes and hosted ComfyUI, so all unknown-price runs keep one
  rule.

Open for the owner: whether a remote endpoint (a self-hosted GPU box on a public address) may
ever be marked free. This record says no; the person enters a small non-zero price instead.
