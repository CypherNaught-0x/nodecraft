## 2026-10-19 · Custom shader actions are one bounded per-pixel WGSL function (#90)

- **What a person (or model) writes**: `fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>`
  plus up to 16 float parameters (name, label, min, max, default), each readable as `name()`.
  The host provides `pixel(x, y)` (clamped source pixel) and `size()`, and wraps the function in
  its own compute shader that reads the source, calls `effect` once per pixel and writes the
  clamped result. Colours are 0–1 RGBA.
- **Validated with naga before any GPU sees it** (`nodecraft_gpu::shader::validate`, naga pinned
  to wgpu's version): the assembled module must parse and validate; the person's code may declare
  no bindings, globals, overrides or entry points, reference no host buffer except through the
  helpers, and contain no loop or barrier; no value may exceed 16 KiB (GPU stacks are small); and
  a static cost (operations, each call counted at its callee's full cost) is at most 100,000 per
  pixel, so call chains can't multiply the work. Source ≤ 32 KiB. The person's source comes first
  so compiler messages carry their own line numbers.
- **Failures never panic.** Every GPU call runs inside validation, internal and out-of-memory
  error scopes, and an error none of them caught marks the runner broken (wgpu's default handler
  would panic). A shader the backend refuses isn't compiled again. Bounded work per pixel still
  takes a while on a huge image: a run over 10 seconds fails, and the next run starts on a fresh
  runner. Shaders run one at a time on one process-wide runner.
- **Stored on the node**: `config["nodecraft.shader"]` as JSON, set with `node.shader_set`
  (validated, one undo step, open to agents: it's free and reversible). The kind's sliders come
  from the declarations; changing the shader changes the input hash, so results go stale and live
  recompute (#88) reruns them.
- **GPU only.** It runs on a process-wide GPU runner made on first use; without a GPU the node
  says so. There is no CPU fallback for arbitrary WGSL.
- **Not yet**: asking a model to write or fix a shader (a paid call through the run path, #301),
  the shader editor in the inspector, and saving shaders to the library.
