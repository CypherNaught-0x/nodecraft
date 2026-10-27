# Workflow starters

Open **Techniques → Starter templates**, find a feature and choose **Add template**. Its canvas
and bundled examples are added to your current project as one undoable edit. Existing work is
preserved. Each starter version can be added once; use Canvases to reopen it, or duplicate its
nodes for another copy. Every graph contains a Note explaining what to edit. Published Technique
versions also show their short descriptions in the project and personal-library lists.
CLI/MCP/control callers use the same `template.list {}` and `template.add {"id":"generate-text"}`
commands. Adding a starter is an undoable project edit and never starts a provider run.

The same files in `assets/templates/*.ncraft` can be opened directly, shared and edited. The
versioned `index.json` records descriptions, feature tags and BLAKE3 file digests. This collection
stays in this repository; #953 separately tracks moving it. All graph data, fixtures and wording
are original NodeCraft material. Tiny bundled PNG/PDF examples are authored by the Rust data
author; the GLB triangle and WAV reuse NodeCraft's authored test fixtures. The one-second blue
32×32 video with a moving orange marker, fixed orientation mark and 440 Hz tone was authored with
FFmpeg, with metadata stripped; its original pixel recipe is in the fixture README. PNGs are opaque
asymmetric multicolor shapes on green so transforms and color keying are observable. No vendor
media or workflow JSON is included.

Cloud templates use actual catalog models and the normal cost/consent path. Configure your own
key before live use. **Switch cloud and local** keeps the same Image card and prompt wire when
swapping fal FLUX Schnell and local ComfyUI SDXL. A local checkpoint and server must be configured
on your own installation. **Mapped core ComfyUI workflow** is an original core-node example, not
one of the third-party curated mappings: offline schema/mapping contracts are tested and no live
server/model validation is claimed. It does not depend on unmerged mapping work.

**Generate a PDF document** uses the available Deck editor and `deck.export` command to create a
new PDF; it is local document creation. PDF sources, extraction and document-aware LLM input have
separate examples. There is currently no dedicated `generation.document` AI card/model; the six
catalog-backed generation kinds are image, text, video, audio, mesh and trained style. This
collection does not invent such a provider. Canvas navigation, menus, connection gestures,
undo/redo, settings, API-key management and deployment are interaction/host capabilities, not
additional executable graph-node kinds; their existing dedicated tests remain authoritative.

## Coverage and verification

The table inventories the currently available workflow/node features. Each archive is opened,
graph/kind/asset-validated, run and round-tripped by
`templates952_every_archive_validates_and_runs_with_mock_providers_and_explicit_mock_native_resources`.
`templates952_inventory_covers_every_reachable_builtin_node_kind_and_all_named_workflows` compares
the actual built-in registry with all shipped graphs so a newly available node needs coverage.
`templates952_add_is_atomic_undoable_rejects_repeats_and_preserves_unsaved_work_and_assets` exercises
the actual insertion command for every template. Expected result modalities/port cardinality
are checked by the production planner/executor; CSV/list/Cross/Zip additionally assert job counts.

| Feature(s) | Template file | Expected behavior |
| --- | --- | --- |
| text-generation | [generate-text.ncraft](../assets/templates/generate-text.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| image-generation | [generate-image.ncraft](../assets/templates/generate-image.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| video-generation | [generate-video.ncraft](../assets/templates/generate-video.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| audio-generation | [generate-audio.ncraft](../assets/templates/generate-audio.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| mesh-generation | [generate-3d.ncraft](../assets/templates/generate-3d.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| cloud-local-switch | [cloud-local.ncraft](../assets/templates/cloud-local.ncraft) | The prompt wire stays connected when changing this Image card from fal FLUX Schnell to local ComfyUI SDXL. |
| document-source, pdf-text, pdf-images | [documents.ncraft](../assets/templates/documents.ncraft) | Extract original PDF text and page images. |
| document-aware-generation | [document-prompt.ncraft](../assets/templates/document-prompt.ncraft) | A PDF feeds a document-aware text model. |
| combine, layer-editor | [combine.ncraft](../assets/templates/combine.ncraft) | Two images feed an edit model and a local Layer Editor for comparison. |
| branch | [branch.ncraft](../assets/templates/branch.ncraft) | One source feeds two independent edits. |
| prompt-helper, image-guided-prompt | [prompt-helper.ncraft](../assets/templates/prompt-helper.ncraft) | Image → text helper → image edit, with the original image also wired into the edit. |
| csv-batch | [csv-batch.ncraft](../assets/templates/csv-batch.ncraft) | 2 item results, in source order |
| prompt-list | [prompt-list.ncraft](../assets/templates/prompt-list.ncraft) | 2 item results, in source order |
| batch-cross | [batch-cross.ncraft](../assets/templates/batch-cross.ncraft) | 4 paired outputs (8 provider calls including preparers) |
| batch-zip | [batch-zip.ncraft](../assets/templates/batch-zip.ncraft) | 2 paired outputs (6 calls including preparers) |
| technique, published-technique, app-form | [technique.ncraft](../assets/templates/technique.ncraft) | A published prompt helper runs as one Technique node. |
| technique, published-technique, app-form | [published-technique.ncraft](../assets/templates/published-technique.ncraft) | The project includes an immutable published version and its editable source workflow. |
| technique, published-technique, app-form | [app-form.ncraft](../assets/templates/app-form.ncraft) | Select the Technique node and open App mode. |
| router | [router.ncraft](../assets/templates/router.ncraft) | A Router is a transparent hub with no provider or cost. |
| mapped-comfy-workflow | [comfy-core.ncraft](../assets/templates/comfy-core.ncraft) | An original six-node core-only sampler workflow. |
| image.blur | [action-image-blur.ncraft](../assets/templates/action-image-blur.ncraft) | Blur runs locally on tiny original examples. |
| image.grade | [action-image-grade.ncraft](../assets/templates/action-image-grade.ncraft) | Color grade runs locally on tiny original examples. |
| image.filter | [action-image-filter.ncraft](../assets/templates/action-image-filter.ncraft) | Filter runs locally on tiny original examples. |
| image.tint | [action-image-tint.ncraft](../assets/templates/action-image-tint.ncraft) | Tint runs locally on tiny original examples. |
| image.flip | [action-image-flip.ncraft](../assets/templates/action-image-flip.ncraft) | Flip runs locally on tiny original examples. |
| image.rotate | [action-image-rotate.ncraft](../assets/templates/action-image-rotate.ncraft) | Rotate runs locally on tiny original examples. |
| image.crop | [action-image-crop.ncraft](../assets/templates/action-image-crop.ncraft) | Crop runs locally on tiny original examples. |
| image.key | [action-image-key.ncraft](../assets/templates/action-image-key.ncraft) | Remove color runs locally on tiny original examples. |
| image.replace_color | [action-image-replace_color.ncraft](../assets/templates/action-image-replace_color.ncraft) | Replace color runs locally on tiny original examples. |
| image.aspect | [action-image-aspect.ncraft](../assets/templates/action-image-aspect.ncraft) | Aspect ratio runs locally on tiny original examples. |
| image.overlay | [action-image-overlay.ncraft](../assets/templates/action-image-overlay.ncraft) | Overlay runs locally on tiny original examples. |
| image.duplicate | [action-image-duplicate.ncraft](../assets/templates/action-image-duplicate.ncraft) | Duplicate runs locally on tiny original examples. |
| image.resize | [action-image-resize.ncraft](../assets/templates/action-image-resize.ncraft) | Resize runs locally on tiny original examples. |
| image.expand | [action-image-expand.ncraft](../assets/templates/action-image-expand.ncraft) | Expand canvas runs locally on tiny original examples. |
| text.split | [action-text-split.ncraft](../assets/templates/action-text-split.ncraft) | Split text runs locally on tiny original examples. |
| text.replace | [action-text-replace.ncraft](../assets/templates/action-text-replace.ncraft) | Replace text runs locally on tiny original examples. |
| text.template | [action-text-template.ncraft](../assets/templates/action-text-template.ncraft) | Template runs locally on tiny original examples. |
| text.concat | [action-text-concat.ncraft](../assets/templates/action-text-concat.ncraft) | Two distinct text sources join in wire order. |
| layout, text-layers, shape-layers | [layout.ncraft](../assets/templates/layout.ncraft) | A Layer Editor holds original text and shape layers at a small canvas size. |
| mesh-source, mesh-convert | [mesh-convert.ncraft](../assets/templates/mesh-convert.ncraft) | An original triangle is kept as GLB. |
| element-reference | [elements.ncraft](../assets/templates/elements.ncraft) | Two images become one named Element reference sheet. |
| storyboard, deck-present, deck-export | [storyboard.ncraft](../assets/templates/storyboard.ncraft) | Editable Deck; PDF export with 1 or 3 pages |
| generate-document, deck-present, deck-export | [generate-document.ncraft](../assets/templates/generate-document.ncraft) | Editable Deck; PDF export with 1 or 3 pages |
| image.mask | [action-image-mask.ncraft](../assets/templates/action-image-mask.ncraft) | The original image feeds an editable local tool. |
| image.shader | [action-image-shader.ncraft](../assets/templates/action-image-shader.ncraft) | The original image feeds an editable local tool. |
| mesh.photo | [action-mesh-photo.ncraft](../assets/templates/action-mesh-photo.ncraft) | An original GLB triangle feeds this local 3D capture. |
| mesh.views | [action-mesh-views.ncraft](../assets/templates/action-mesh-views.ncraft) | An original GLB triangle feeds this local 3D capture. |
| mesh.turntable | [action-mesh-turntable.ncraft](../assets/templates/action-mesh-turntable.ncraft) | An original GLB triangle feeds this local 3D capture. |
| video.trim | [action-video-trim.ncraft](../assets/templates/action-video-trim.ncraft) | Trim video edits an original one-second blue clip with a tone. |
| video.speed | [action-video-speed.ncraft](../assets/templates/action-video-speed.ncraft) | Change speed edits an original one-second blue clip with a tone. |
| video.stitch | [action-video-stitch.ncraft](../assets/templates/action-video-stitch.ncraft) | The original rightward-moving clip precedes its reversed clip; the result lasts two seconds. |
| video.replace_audio | [action-video-replace_audio.ncraft](../assets/templates/action-video-replace_audio.ncraft) | Replace sound edits an original one-second blue clip with a tone. |
| video.frames | [action-video-frames.ncraft](../assets/templates/action-video-frames.ncraft) | Extract frames edits an original one-second blue clip with a tone. |
| video.contact_sheet | [action-video-contact_sheet.ncraft](../assets/templates/action-video-contact_sheet.ncraft) | Contact sheet edits an original one-second blue clip with a tone. |
| video.long_exposure | [action-video-long_exposure.ncraft](../assets/templates/action-video-long_exposure.ncraft) | Long exposure edits an original one-second blue clip with a tone. |
| video.reverse | [action-video-reverse.ncraft](../assets/templates/action-video-reverse.ncraft) | Reverse video edits an original one-second blue clip with a tone. |
| video.ken_burns | [action-video-ken_burns.ncraft](../assets/templates/action-video-ken_burns.ncraft) | Pan and zoom edits an original one-second blue clip with a tone. |
| video.grade | [action-video-grade.ncraft](../assets/templates/action-video-grade.ncraft) | Grade video edits an original one-second blue clip with a tone. |
| video.filter | [action-video-filter.ncraft](../assets/templates/action-video-filter.ncraft) | Video effect edits an original one-second blue clip with a tone. |
| video.key | [action-video-key.ncraft](../assets/templates/action-video-key.ncraft) | Key out a colour edits an original one-second blue clip with a tone. |
| video.watermark | [action-video-watermark.ncraft](../assets/templates/action-video-watermark.ncraft) | Watermark video edits an original one-second blue clip with a tone. |
| video.duplicate | [action-video-duplicate.ncraft](../assets/templates/action-video-duplicate.ncraft) | Duplicate video edits an original one-second blue clip with a tone. |
| video.split | [action-video-split.ncraft](../assets/templates/action-video-split.ncraft) | Split video edits an original one-second blue clip with a tone. |
| video.extract_audio | [action-video-extract_audio.ncraft](../assets/templates/action-video-extract_audio.ncraft) | Extract sound edits an original one-second blue clip with a tone. |
| timeline | [timeline.ncraft](../assets/templates/timeline.ncraft) | A one-second original clip becomes an automatic Timeline clip. |
| style-training, style-application | [train-style.ncraft](../assets/templates/train-style.ncraft) | Four references train a reusable style; a compatible image card applies it. |
| folder, folder-filter, folder-range | [folder.ncraft](../assets/templates/folder.ncraft) | 2 item results, in source order |
| edit-image | [edit-image.ncraft](../assets/templates/edit-image.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| image-to-video | [image-to-video.ncraft](../assets/templates/image-to-video.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| image-to-3d | [image-to-3d.ncraft](../assets/templates/image-to-3d.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| transcribe-audio | [transcribe-audio.ncraft](../assets/templates/transcribe-audio.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| generation-sheet, variations | [generation-sheet.ncraft](../assets/templates/generation-sheet.ncraft) | Two Image cards share a sheet; the first makes two variations, the second one (three requests). |

Portable CI uses the existing MockProvider for every generation request. Its intentionally
signature-only media are replaced with our authored codec-valid fixtures before production
output conversion. Deterministic image/text/PDF actions execute their actual implementation.
GPU/FFmpeg actions use an explicit mock-resource profile for portable graph/execution validation;
this does **not** claim their pixels or timing were produced by a GPU or FFmpeg.

The ignored offline test `templates952_native_resources_execute_the_real_video_and_gpu_actions`
executes those real implementations on a native GPU with `NODECRAFT_VIDEO_TEST_FFMPEG` and
`NODECRAFT_VIDEO_TEST_FFPROBE` set to authorized executable paths. It never calls a provider or
ComfyUI server. `templates952_pdf_creation_and_storyboard_export_generate_valid_complete_documents`
exports actual PDFs and opens them with the document decoder. The model-swap and duplicate-path
tests verify unchanged wiring and Deck reference remapping. App/library UI tests exercise actual
pointer insertion, descriptions, undo and both theme captures.

The real native profile exposed and fixed Stitch videos passing a folded input list as one
clip. Its action planner now supplies the list's clips in order; scalar feeds retain their
order. `templates952_stitch_plans_collection_items_in_order_and_keeps_scalar_feeds` is the
old-code-failing regression; empty collections refuse before a local video job is emitted.
Decoded-pixel tests prove opacity, asymmetric transforms, blur differences and green-key alpha;
native frame positions prove rightward original motion, leftward reverse and forward-then-reverse
join order. The native profile checks the two-second stitched duration, while portable tests
check two typed clip inputs and both distinct joined text values.

To re-author graphs after an intentional data or schema change, run the ignored Rust
`templates952_author::templates952_author` test with `TEMPLATE952_WRITE_DIR` set to the absolute
repository `assets/templates` path, then review the graph/index diff and run the template tests.
It performs no generation, network request or credential read. No screenshots are committed.
