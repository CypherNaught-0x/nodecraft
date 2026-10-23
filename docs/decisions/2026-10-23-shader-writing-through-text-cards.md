## 2026-10-23 · A model writes a custom shader through an ordinary text card (#90)

- **No hidden paid call.** `shader.write` only adds a text card (`gen.text`, titled "Shader
  request", left of the shader card) whose prompt is the shader contract, the current shader and
  the request or the compiler's message. Running it is a normal run: quoted first, inside budgets,
  with idempotency keys and a history entry. The card stays on the canvas, so what was asked, what
  it cost and what came back can be read, branched and rerun like any other result. Adding it is
  free and one undo step. The model is the request's, else the catalog's first text model.
- **Its answer is stored by a separate, free command.** `shader.apply {canvas, id, from}` reads the
  card's active text answer, takes the first fenced code block (or the whole answer), turns its
  `// param: name | Label | min | max | default` lines into declared parameters and stores the
  result through the same validated path as `node.shader_set` (one undo step). An answer that
  doesn't parse or validate is refused with the reason, which is exactly what a fix request sends
  back to the model. Agents and the CLI drive the same two commands; nothing is applied
  automatically outside the editor.
- **The prompt shows the shader in the answer's own format** (source first, so compiler line
  numbers match, then the parameter lines), so a model that changes one line returns a reply that
  reads back as the same shader plus that change.
- **The editor** (card menu → Edit shader…) edits the WGSL and the parameter rows, checks the draft
  with `nodecraft_gpu::shader::validate` as it's typed, and stores it with Apply. Write/Fix with a
  model calls `shader.write` and then asks to run the new card (the price confirmation shows
  first); when that card's active answer changes, the editor calls `shader.apply` and shows the
  stored shader or the refusal, with Fix offered again.
- **Compiler reports use ASCII framing.** naga's box-drawing characters are mapped to `- | +`,
  since the UI font and many terminals lack them.
- **Not yet**: saving shaders to the library and versioning saved actions; shaders with more than
  one image input (#162).
