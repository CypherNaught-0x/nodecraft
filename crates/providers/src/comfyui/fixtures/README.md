# ComfyUI 3D contract fixtures

`triangle.gltf` is an original asymmetric triangle with its vertex/index bytes embedded. The
GLB/OBJ triangles and PNG poster in tests are also authored locally, without vendor media.

`mesh-object-info.json` is an authored **documentation projection**, not a captured server
response. It describes only the specific core interfaces used by the original single-view
Hunyuan3D preset. The saver describes the MESH variant; newer documentation also describes
FILE3D inputs. Public references (read 2026-10-27):

- https://docs.comfy.org/tutorials/3d/hunyuan3D-2
- https://docs.comfy.org/built-in-nodes/ImageOnlyCheckpointLoader
- https://docs.comfy.org/built-in-nodes/ClipVisionEncode
- https://docs.comfy.org/built-in-nodes/Hunyuan3Dv2Conditioning
- https://docs.comfy.org/built-in-nodes/EmptyLatentHunyuan3Dv2
- https://docs.comfy.org/built-in-nodes/VAEDecodeHunyuan3D
- https://docs.comfy.org/built-in-nodes/VoxelToMesh
- https://docs.comfy.org/built-in-nodes/SaveGLB
- https://docs.comfy.org/built-in-nodes/ModelSamplingAuraFlow

The node reference pages identify themselves as AI-generated. Their prose documents interfaces,
not an actual installed schema, compatible checkpoint or successful run. No ComfyUI implementation
or vendor workflow file was copied. The recipe's Euler/normal, 30 steps, CFG 5, 3072 latent and
2.1 shift 1.0 are our inferred starting settings, not claimed official recommendations.
Actual server/model/version/output/viewer validation remains a merge blocker.
