## 2026-10-20 · Option lists are fetched on request, by a fixed source table (#529)

- **A fixed table maps source ids to ComfyUI inputs.** `comfyui.checkpoints` is the choice list
  of `CheckpointLoaderSimple.ckpt_name`; `comfyui.loras`, `comfyui.vaes`,
  `comfyui.upscale_models`, `comfyui.samplers` and `comfyui.schedulers` read `LoraLoader.lora_name`,
  `VAELoader.vae_name`, `UpscaleModelLoader.model_name` and `KSampler`'s `sampler_name` and
  `scheduler`. These are the stock loader nodes every ComfyUI install has, so a source id means
  the same list on every server; a catalog row can't point a source at an arbitrary class and
  input, which keeps the ids stable for contracts and agents. A new source is a row in
  `crates/engine/src/option_fetch.rs` and a line in `docs/descriptor-contract.md`.
- **Fetched on request, never on a timer.** `options.refresh` is a command whose handler does no
  I/O; the host fetches through the same `provider_setup::comfyui` the run adapter uses, so the
  two can't read the server's address, salt or token differently. A background refresh policy
  belongs with the servers panel (#9).
- **Refused whole, never truncated.** A list over the store's 16,384 values is refused with a
  reason and the previous list kept: a truncated list would refuse values the server has.
  Empty, oversized and repeated values are dropped and counted instead, since one of them would
  otherwise lose the whole list.
- **Once per model.** Lists are keyed by the contract's namespace, so the same checkpoint list is
  installed for every model that names it; past the store's limits the rest are reported as
  refused, never silently dropped.
- **Scoped sources wait.** A list narrowed by another parameter (a checkpoint's LoRAs) needs a
  fetch per scope value; it is reported as refused until one exists.
