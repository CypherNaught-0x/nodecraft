## 2026-10-20 · A value off a fetched option list blocks the run (#162)

- **What changed.** Run planning now consults the option lists the host has installed, so
  `not_an_option` blocks the run before a job is admitted, priced or charged and before an
  idempotency key exists. This narrows the last bullet of
  [the option-sources record](2026-10-20-option-sources.md); everything else in it stands, and
  this issue fetches nothing. The host that fills `OptionLists` from a real server is still the
  ComfyUI `object_info` and provider-inventory issues.
- **One rule, one place.** Planning asks `NodeContract::resolve_with(values, ready, options)` —
  the resolver every one of the edit, bulk-edit, describe and `node.options` paths already asks,
  with `ready` set here and in `node.describe` — and treats a `not_an_option` issue as a blocker.
  Unknown-is-not-empty, disabled-parameters-draw-from-no-list and scope narrowing therefore
  cannot drift between the surfaces; the run and the inspector report the same issue for the
  same node.
- **Where it consults, and where it deliberately does not.** `run.quote` and `run.start` plan
  through `planner::plan_expanded` with the session's lists. Live local recompute and a
  technique's publish check keep planning with `NoOptions` (`plan`/`plan_with_targets`, and
  `plan_expanded` given `NoOptions` for the publish check): neither admits a paid job, and refusing a free local recompute against a list would
  be a new way for a stale server answer to stop someone's canvas. Planning stays pure — the
  lookup arrives as a parameter, like the endpoint view does.
- **Technique nodes are checked too.** `run.quote` and `run.start` plan through
  `planner::plan_expanded`, which inlines placed techniques (#94) before planning, with the same
  lookup; the inlined nodes carry the version's params, so an off-list value inside a technique
  blocks the run. The blocker is reported on the technique node with the same `not_an_option`
  code rather than folded into `node_validation`, so an agent acts on it the same way.
- **The report, not the message.** `Reason::NotAnOption` carries nothing. The only report a run
  produces is `blocked_reasons`, and its rule is fixed-shape records: a UUID, a code and static
  text, never a native hook string, node title, parameter name or model value. It gets a new
  `not_an_option` code, which is what a shell, CLI or agent acts on; naming the field is
  `node.describe` and `node.options`, which already report the same `not_an_option` issue with its
  parameter on the same node. A `Blocker::message` that named the parameter would reach nobody on
  a run anyway: its only non-test caller is the technique publish check, which plans with no
  lists. Putting the parameter into the JSON would break that leak invariant for one string a
  second command already answers, so it was rejected.
- **A stale value is still fixable.** Because the refusal lands at planning, the node stays
  editable around it and the stale value can be cleared or replaced, exactly as the
  option-sources record intended; a refresh that removes a value no longer turns into a paid job
  the provider then refuses.
- **Two residuals, both accepted.** A list that becomes known *during* a run does not stop the
  deferred jobs of a step already planned: planning decided once, and the provider stays the
  authority for a step that was already in flight. And one off-list node stops the whole run it
  belongs to, including an `all_stale` or field run that would otherwise have made everyone else's
  results — the same blast radius a missing required parameter already has, and the reason the
  blocked report names every node it refused.
- **Cost.** A step whose contract names no list pays one pass over its rules and no resolution.
  A step that names one pays a second resolution beside the one `validate_instance` already made,
  over that node's parameters, once per plan.
