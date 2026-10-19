## 2026-10-19 · Custom shader actions are one bounded per-pixel WGSL function (#90)

- **What a person (or model) writes**: `fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>`
  plus up to 16 float parameters (name, label, min, max, default), each readable as `name()`.
  The host provides `pixel(x, y)` (clamped source pixel) and `size()`, and wraps the function in
  its own compute shader that reads the source, calls `effect` once per pixel and writes the
  clamped result. Colours are 0–1 RGBA.
- **Validated with naga before any GPU sees it** (`nodecraft_gpu::shader::validate`, naga pinned
  to wgpu's version): the assembled module must parse and validate; the person's code may declare
  no bindings, globals, overrides or entry points, reference no host buffer except through the
  helpers, and contain no loop (so every invocation finishes) or barrier; source ≤ 32 KiB. The
  person's source comes first so compiler messages carry their own line numbers.
- **Stored on the node**: `config["nodecraft.shader"]` as JSON, set with `node.shader_set`
  (validated, one undo step, open to agents: it's free and reversible). The kind's sliders come
  from the declarations; changing the shader changes the input hash, so results go stale and live
  recompute (#88) reruns them.
- **GPU only.** It runs on a process-wide GPU runner made on first use; without a GPU the node
  says so. There is no CPU fallback for arbitrary WGSL.
- **Not yet**: asking a model to write or fix a shader (a paid call through the run path, #301),
  the shader editor in the inspector, and saving shaders to the library.
