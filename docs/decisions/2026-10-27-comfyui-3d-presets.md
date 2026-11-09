## 2026-10-27 · Core-node ComfyUI 3D presets retain the mesh output contract (#24)

- Add original single-image Hunyuan3D 2.0/2.1 graphs at the existing ComfyUI template seam.
  Keep the existing provider feature, server routing, upload checks, queue, saved handles and
  256-MiB file/aggregate limits. No new scheduler or custom-node implementation is bundled.
- The catalog requires one Image and returns Mesh. These shape models have no prompt input
  and produce untextured geometry. Multi-view and plugin workflows use explicit imported
  workflows; there is no undocumented TRELLIS plugin preset.
- The image-only checkpoint loader has its own fixed option source. Server schemas and
  installed choices are checked before uploads or queueing, including a single stale-catalog
  refresh. A missing node or model is an actionable refusal.
- Durable handles for these named presets record their Mesh-only result contract, including
  idempotent reused submissions and cloud handles. Posters and texture images cannot become
  unexpected Image-port results. Native workers decode meshes within existing media limits
  before success, retaining their exact original bytes. Browser polling retains recognized
  originals and uses the existing small-model preview boundary, without adding a large inline
  decode. Generic/legacy workflow handles keep their existing mixed-output behavior.
- Public node documentation is an interface projection, not a captured installed schema.
  The original recipe's inference settings are provisional. Real server/model/version/output
  and viewer validation remains a merge blocker; see [the validation record](../comfyui-3d.md).
