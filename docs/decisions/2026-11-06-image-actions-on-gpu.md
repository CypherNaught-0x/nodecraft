## 2026-11-06 · Built-in image actions run on the GPU in the desktop app (#1058)

`nodecraft_gpu::image` (#89) has computed every built-in image request on wgpu, tested against
the CPU reference, but nothing used it. Now the desktop app does.

- **The desktop app opts in; nothing else does.** `apps/nodecraft` calls
  `action_exec::image_gpu::enable(true)` at start. The CLI, `serve`, the MCP server and the
  browser keep the CPU reference, so their results stay byte-exact and reproducible on any machine.
  `NODECRAFT_IMAGE_GPU=0` keeps the desktop app on the CPU too.
- **The GPU is an accelerator, the CPU the reference.** One executor, made on first use, serves
  actions in turn. An action goes to the CPU when the GPU is busy with another, when there is no
  compatible adapter (looked for again a minute later), when the device fails or loses its
  mapping (the next action asks for a fresh one), when the GPU refuses the request, when it takes
  longer than 30 s, or when the action is cancelled while it waits. Requests the CPU would refuse
  are left to it, so its error is the one shown.
- **Tolerance.** A GPU result matches the CPU reference within 2 in every 8-bit channel of
  straight colour; where either alpha is at most 2 the colour is compared premultiplied, within
  2/255. `nodecraft_gpu::image` tests every request family against that; the engine's own ignored
  `on_a_gpu_every_family_matches_the_cpu_reference` checks the routed families, and runs on a
  software Vulkan driver (lavapipe) as well as real GPUs.
- **Same budgets.** A GPU result is charged the CPU reference's work for that request, so a live
  round's work budget counts it the same way; one that falls back is charged by the CPU as it
  goes. The executor's own resource budget bounds what it allocates.
- **Covered actions.** Blur, grade, filter, tint, flip, rotate, crop, key, replace colour,
  aspect, overlay, duplicate and resize. Expand (`image.expand`) is a padded copy with no compute
  and stays on the CPU, as do SVG rasterisation and masks.
- **A given-up action doesn't switch the GPU off.** One cancelled or timed out leaves its
  submission in flight; the next action drives the device until it finishes and tries again, so
  live recompute's frequent cancels don't leave the session on the CPU.
- **Who sees GPU pixels.** Only the desktop app, including agents and control clients driving
  it. Caching keys on inputs, so a GPU result reused later is the same result, not a recomputation.
