## 2026-10-20 · Run-time option sources: unknown refuses nothing (#162)

- **Contract 1.1.** `ParamRule.options: {id, scope?}` names a list known only at run time. It
  is optional and absent means the 1.0 meaning, so it is a minor version. Only single-line text
  and model parameters take one; `scope` must be an earlier single-line text, choice or model
  parameter, so lists cannot depend on each other in a cycle.
- **The host owns the lists.** Fetched lists live in a bounded `OptionLists` session resource,
  not in the contract or the project, because they describe a server or an account and change
  without the document changing. Keys carry the contract's namespace, so two models never share
  a list by accident.
- **Unknown is not empty.** An unfetched list, a failed fetch or a scope with no value refuses
  nothing; the provider stays the authority. Treating unknown as empty would make every node
  uneditable while offline.
- **Off-list never blocks the node.** `not_an_option` is dropped when not `ready`, and edit
  commands refuse it only when they set that parameter (clearing it is allowed). A checkpoint deleted on the server
  then shows as an issue on its own parameter while the rest of the node stays editable.
- **Run planning consults the installed lists.** It did not when this record was written;
  [A value off a fetched option list blocks the run](2026-10-20-option-lists-block-a-run.md)
  is the change that made it so.
  `run.quote` and `run.start` plan with the host's `OptionLists`, so a value off a *known* list
  blocks the run before anything is admitted, priced or charged. What still refuses nothing is
  what never did: a list the host has not fetched, could not fetch, or whose scope parameter has
  no value, so a run is never refused against a list the host never refreshed. Fetching from a
  real server (ComfyUI `object_info`, provider inventories) remains a separate issue.
