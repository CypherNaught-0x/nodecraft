# ComfyUI 3D workflows

NodeCraft maps mesh-producing workflow savers to Mesh list ports. Saved GLB, embedded-buffer
glTF and OBJ files come back through the configured ComfyUI server's normal history and file
routes. The ordinary 3D viewer, extraction, original-file export and project storage use those
assets; preview or texture PNGs are not substitutes for a mesh.

## Local Hunyuan3D shape presets

| Catalog model | Installed checkpoint | Input | Output |
| --- | --- | --- | --- |
| `comfyui.hunyuan3d-2` | `hunyuan3d-dit-v2.safetensors` | One image | Untextured GLB |
| `comfyui.hunyuan3d-2.1` | `hunyuan_3d_v2.1.safetensors` | One image | Untextured GLB |

Install the matching checkpoint under the server's `models/checkpoints` and use a version of
ComfyUI with the required core nodes. The [official tutorial](https://docs.comfy.org/tutorials/3d/hunyuan3D-2)
describes these model files and geometry-only output. No model weights, vendor workflows,
prompts or example media ship in NodeCraft. The presets have no text prompt or texture stage.
NodeCraft quotes local GPU work as free; your server may have its own hosting costs. A hosted
Comfy Cloud profile follows its normal quote/consent policy.

**Refresh option lists** reads `ImageOnlyCheckpointLoader.ckpt_name` for these models, separately
from FLUX/SDXL's `CheckpointLoaderSimple` list. The adapter checks the entire graph against the
server's current node definitions before any upload, including missing nodes, connection types,
parameter ranges and installed checkpoint choices. A stale catalog gets one refresh. It does
not substitute another checkpoint or silently ignore an extra prompt/image.

The original graphs connect image encoding, Hunyuan conditioning, an empty latent, sampling,
VAE voxel decoding, surface extraction and SaveGLB. Their starting settings are **our inferred
recipe**, not claimed official Hunyuan recommendations: Euler/normal, 30 steps, CFG 5, 3072
latent resolution, batch one, 8000 decoder chunks, octree 256 and threshold 0.6. The 2.1 recipe
also applies an AuraFlow sampling shift of 1.0. Steps, CFG and seed are editable; a random seed
is derived stably from the job identity. Live validation must establish or correct this recipe.

Named preset handles retain their Mesh-only output policy across save/resume. Poster and
texture images are excluded from their results. Native runs validate meshes with bounded media
decoding off the frame thread. A poster-only, damaged native mesh or oversized result is an explicit error. Original mesh bytes
are retained. Per-file and aggregate downloads remain capped at 256 MiB, with at most 512 files.
Generic imported workflows keep their mapped Image/Mesh outputs and selected saver ordering.
The browser retains the recognized original and uses the existing small-model preview limits;
it does not decode a large downloaded mesh in its polling frame. An invalid or unsupported
browser preview has an explicit unavailable state. A browser geometry worker remains #569.

## Other mesh workflows

Import API-format or UI-format workflows with **Import ComfyUI workflow** after reading that
server's node definitions. Expose required images/fields and select the mesh saver. Core
Hunyuan multi-view and other mesh-producing plugins can use that path when their installed
schema and saved formats match. No TRELLIS plugin/version compatibility is claimed by these
two named presets. Missing custom nodes are reported before queueing.

Generation cards retain their common prompt field for switching models. These two image-only
presets do not transmit that text or use it as conditioning; the wired image drives the shape.

## Validation record and merge blocker

**Live validation is outstanding and blocks merging this change.** No configured server was
probed and no generation request was made for this work. Record actual ComfyUI version, model
checkpoint/hash, any custom-node versions, run settings, output format/count and viewer result
here after an authorized live run. Do not treat projected fixtures or a catalog-only read as
that evidence.

The authored `mesh-object-info.json` fixture projects public node interface documentation.
Those primary-hosted node pages explicitly identify themselves as AI-generated; they are not
captured `/object_info`. In particular, the fixture models the MESH saver variant, while current
SaveGLB documentation also describes FILE3D inputs. Fixture provenance and exact public links
are in `crates/providers/src/comfyui/fixtures/README.md`.

Offline validation covers original triangle GLB/OBJ/embedded glTF data, queue/history/file
handling, exact bytes, mixed poster/mesh outputs, bad settings, missing model/node refusals,
saved handles and caps. Full engine/UI proof and gate results are recorded with the PR; the
feature is not declared live-validated by this document.

An authorized operator with the installed checkpoint can run the opt-in generating test:

```sh
NODECRAFT_LIVE_COMFYUI_3D=1 NODECRAFT_COMFYUI_3D_IMAGE=/path/to/public-safe.png \
  cargo test -p nodecraft-providers --test live_comfyui comfy3d24_live_shape_output -- --ignored
```

`NODECRAFT_COMFYUI_URL` selects an unauthenticated test server; the default is local port8188.
`NODECRAFT_COMFYUI_3D_MODEL` selects `hunyuan3d-2.0` or `hunyuan3d-2.1` (the default).
This test queues one generation and verifies decoded mesh results. It does not certify the
interactive viewer or record server/model versions automatically; retain those in the record
above after testing. This test has not been run for this change.
