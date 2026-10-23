## 2026-10-23 · The ComfyUI workflow card stores its workflow and runs on a free model (#9)

An imported ComfyUI workflow becomes one card of kind `generation.comfy_workflow`. Like a
placed Technique, the card's ports and parameters come from its config rather than from the
catalog:

- `comfy.workflow.version` is `1`.
- `comfy.workflow.v1.api` is the API-format workflow, at most 960 KiB.
- `comfy.workflow.v1.mapping` is a versioned `Mapping` that refuses unknown fields. It lists
  which loader inputs are ports, which literal inputs are parameters (each with a full
  `ParamSpec`), and which output nodes give results.

A card made by a newer NodeCraft is refused as unsupported rather than guessed at.
`nodecraft_nodes::comfy_workflow::propose` builds this config from `nodecraft_comfy::discover`.
`comfy.workflow.import` places a workflow file as a card. It reads the file against the
server's node list, which the host fetches with `comfy.nodes.refresh` and the session keeps. A
failed read keeps the list from before. By default it exposes the workflow's prompts and seeds
as parameters; `params` names others. A workflow that uses a node the server lacks is refused,
with those nodes named.

The card runs through the existing run path, not a new one. It has a `model` parameter that
defaults to the catalog model `comfyui.workflow` (provider `comfyui`, `provider_model =
"workflow"`, free, no parameters). So the run picker routes it, quotes cost 0 and admission
checks it like any generation card, with no change to exec. Its job has empty parameters and
three kinds of inputs:

- the workflow text under the role `comfy.workflow`;
- the parameter values as JSON text under `comfy.values`;
- each wired file under `<node>.<input>`.

The ComfyUI adapter treats such a job as carrying its own workflow, never a built-in template.
It bounds both texts, accepts only scalar values, and checks everything before uploading.

A new catalog key, `cards = false`, keeps this model out of pickers, model selection and the
Generate cards. It belongs to one card kind and would be meaningless on others.

Results are routed by media kind, so the card has one list output port per kind of media its
output nodes save (`image`, `video`, `audio`, `mesh`).

The job also names the output nodes the card keeps (`comfy.outputs`). The adapter drops the
workflow's other output nodes before queueing, so a saver the card has no port for never runs and
never fails the step. A card refuses to run while one of its file ports is unwired, rather than
silently loading whatever file the workflow's author last used.

Known limits:

- Seeds are not re-randomised per run.
- Text can't be wired into a parameter.
- Each run's history records the workflow text, as it records prompts.
