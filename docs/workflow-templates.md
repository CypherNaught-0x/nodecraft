# Workflow starters

Open **Techniques → Starter templates**, find a feature and choose **Add template**. Its canvas
and bundled examples are added to your current project as one undoable edit. Existing work is
preserved. Each starter version can be added once; use Canvases to reopen it, or duplicate its
nodes for another copy. Every graph contains a Note explaining what to edit. Published Technique
versions also show their short descriptions in the project and personal-library lists.
CLI/MCP/control callers use the same `template.list {}` and `template.add {"id":"generate-text"}`
commands. Pin `version` and `blake3` from the list to reproduce an exact selection. Adding a starter
is one undoable project edit and never starts a provider run.

The authoritative files live in [nodecraft-templates](https://github.com/CypherNaught-0x/nodecraft-templates).
Its README documents authoring, licensing, validation and publication; SOURCE.md preserves the
migration provenance and filtered #952 history. Ordinary `.ncraft` files can still be opened directly,
shared and edited. The index requires `schema_version: 1`, descriptions, positive template versions,
feature tags and BLAKE3 digests at immutable paths. All graph data, fixtures and wording are
original NodeCraft material. Tiny bundled PNG/PDF examples are authored by the Rust data
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
| text-generation | [generate-text.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generate-text/1/f7683d0272c11f8be693f16eeaab6a746497eeb78468631818ddc0b7b86832d2.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| image-generation | [generate-image.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generate-image/1/f533269f6a24b61bd2c4e114d71f0cad2e2d95a084d8edb2791f1cfda3604f4b.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| video-generation | [generate-video.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generate-video/1/ef2adeece459e506d5d2df17d3ff391dd3a930289b93a49afc9b5af2cc564b79.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| audio-generation | [generate-audio.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generate-audio/1/d80ca9f52a2040d6c526e5389b9038aab2c8bb1ac44d52ec21366d302ead3b42.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| mesh-generation | [generate-3d.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generate-3d/1/6ed086f13d257cd21f6fbde73673d90d749ec7ca7a688d01a95d690323027c40.ncraft) | Change the source prompt, choose a compatible model, review the cost and run the connected generation card. |
| cloud-local-switch | [cloud-local.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/cloud-local/1/44e4f1664dc887a5c9860eb90208c2ae153d3d735963b310534edf2796f5bc21.ncraft) | The prompt wire stays connected when changing this Image card from fal FLUX Schnell to local ComfyUI SDXL. |
| document-source, pdf-text, pdf-images | [documents.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/documents/1/1defa20582b63969ab48e5878af7dbd94f23d01db84cca31c19aea9e8a0b2f96.ncraft) | Extract original PDF text and page images. |
| document-aware-generation | [document-prompt.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/document-prompt/1/22dcdd598cd560f9fcfd5ab9687733026f92e1af55b559412933d35b058fbf65.ncraft) | A PDF feeds a document-aware text model. |
| combine, layer-editor | [combine.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/combine/1/c7f354b9f6ca9bae5a212fdca8e228fc7f02c2a68d6081b099b038de0807ded8.ncraft) | Two images feed an edit model and a local Layer Editor for comparison. |
| branch | [branch.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/branch/1/7da6241698fb8ba713b6e21461f1f07d364a451271cf9856f72be85a0a7e3989.ncraft) | One source feeds two independent edits. |
| prompt-helper, image-guided-prompt | [prompt-helper.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/prompt-helper/1/77a557b32f8fd4c35e80916b2df527543e1312f2ecb1237c4c16744d50f73c62.ncraft) | Image → text helper → image edit, with the original image also wired into the edit. |
| csv-batch | [csv-batch.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/csv-batch/1/589c67cc30d391ebbbf53b03bc99d5bfbc414fe8b17fd23d014a4f2451ea00b4.ncraft) | 2 item results, in source order |
| prompt-list | [prompt-list.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/prompt-list/1/1af9549e26e5c46cd7b58e6e52068db0802873e7d2f9dfd9c76b00395ccb6ca2.ncraft) | 2 item results, in source order |
| batch-cross | [batch-cross.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/batch-cross/1/14cd3e4b175b77b226485819d1a636fa7c0a1df61cb045be0b3f638ce5682f66.ncraft) | 4 paired outputs (8 provider calls including preparers) |
| batch-zip | [batch-zip.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/batch-zip/1/36f3f9eaa06abec36e3173e18ba911154cbb57c68218b6217cf46fdaaf4a3123.ncraft) | 2 paired outputs (6 calls including preparers) |
| technique, published-technique, app-form | [technique.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/technique/1/b3e3277ef966bf5e094e583d3713706bb8ca3f709563eec3d040613c1a05a9b5.ncraft) | A published prompt helper runs as one Technique node. |
| technique, published-technique, app-form | [published-technique.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/published-technique/1/3159a9fed09659f16d6436ad2b954b841efdc6f334e4435f59aec6fc7d526601.ncraft) | The project includes an immutable published version and its editable source workflow. |
| technique, published-technique, app-form | [app-form.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/app-form/1/4402519b3d7602f38e771cca591395cc867a8215ba78a32d002ee18269cc10de.ncraft) | Select the Technique node and open App mode. |
| router | [router.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/router/1/4bf32452ff9b44ecf75e4fb43d031efa4ecbe345ae1bb4500450818f327fb74b.ncraft) | A Router is a transparent hub with no provider or cost. |
| mapped-comfy-workflow | [comfy-core.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/comfy-core/1/438d9e5974f5d8949a18265781930af84a96023acf4bbefb016fb9c68a4bcff2.ncraft) | An original six-node core-only sampler workflow. |
| image.blur | [action-image-blur.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-blur/1/a1970a5b94ae79978217d3e6de38203848e57587874fd7f47b0c1f48ab2f9232.ncraft) | Blur runs locally on tiny original examples. |
| image.grade | [action-image-grade.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-grade/1/d6742275207b8ba007d7c0203e34d75d2720ffdd13553778de83ad8dd39b2246.ncraft) | Color grade runs locally on tiny original examples. |
| image.filter | [action-image-filter.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-filter/1/c5059c3d170e116181031070e89561f3a97f3c002087771fd3298824191c5d2a.ncraft) | Filter runs locally on tiny original examples. |
| image.tint | [action-image-tint.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-tint/1/cb96c50150053596c1634f32770883376434a27c5abdcf0b9371752319eebf4f.ncraft) | Tint runs locally on tiny original examples. |
| image.flip | [action-image-flip.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-flip/1/fe55adb653639c1abbf0584b7c214ae945ae73e446e4eacefa53b852feedac46.ncraft) | Flip runs locally on tiny original examples. |
| image.rotate | [action-image-rotate.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-rotate/1/193b978eb7d33fd27079cde0f5bec62074a1eb8253be9f9aa01c012a58801b08.ncraft) | Rotate runs locally on tiny original examples. |
| image.crop | [action-image-crop.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-crop/1/a2dd084ce0a677d05b242d66e138f88cf8e2b5111de1b8be63f78dbfe29f4a9c.ncraft) | Crop runs locally on tiny original examples. |
| image.key | [action-image-key.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-key/1/1d6e98f1adb424d2c7c7a127d9882f1a9fcd4048a8e31322384d27cf77f30482.ncraft) | Remove color runs locally on tiny original examples. |
| image.replace_color | [action-image-replace_color.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-replace_color/1/f388f08819064097db85409f9f4bae731a5ea030af7ac6967f2563c8ebd480eb.ncraft) | Replace color runs locally on tiny original examples. |
| image.aspect | [action-image-aspect.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-aspect/1/47ea64558345b6dc824abe8b5f1290a4b54daaf71290b1278452b213cfd53b68.ncraft) | Aspect ratio runs locally on tiny original examples. |
| image.overlay | [action-image-overlay.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-overlay/1/ab69e3d10df2a6ea39fa90eca018912e0b9c7d43efd0753c593959d8157c9bec.ncraft) | Overlay runs locally on tiny original examples. |
| image.duplicate | [action-image-duplicate.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-duplicate/1/f7d23c037540966341505de7cd4df6ff72d81971794dec2d97e0d978c87d422d.ncraft) | Duplicate runs locally on tiny original examples. |
| image.resize | [action-image-resize.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-resize/1/2b2ece1a25e5dac991563e46612706e7d01b019665e7ff22904f83c9afaf9538.ncraft) | Resize runs locally on tiny original examples. |
| image.expand | [action-image-expand.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-expand/1/8483390ab532b77d9a65a3960b7cd521c96f0e88477cf877f465ecfc62e5dd0c.ncraft) | Expand canvas runs locally on tiny original examples. |
| text.split | [action-text-split.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-text-split/1/a9492f9610dc4c6e69c1ca82d255e1d1e5d949299227c6c370f5f7e4f6564434.ncraft) | Split text runs locally on tiny original examples. |
| text.replace | [action-text-replace.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-text-replace/1/9fcf78417b2b00647931d3b9efa5f7c0e52256416e339a97c46ec2f527cac1bc.ncraft) | Replace text runs locally on tiny original examples. |
| text.template | [action-text-template.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-text-template/1/2d5bd96270f2a342e4b67223abe8273f9f04348431458196fb7367fda7752314.ncraft) | Template runs locally on tiny original examples. |
| text.concat | [action-text-concat.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-text-concat/1/3795175c87322c0079437521d9bfb8231ca2e237f2c27ba4db9a8ea426b45879.ncraft) | Two distinct text sources join in wire order. |
| layout, text-layers, shape-layers | [layout.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/layout/1/3ba40f0c43e73605e3918953bbc05852b4b45a8cc90abbc1ca4adc99bd4a624d.ncraft) | A Layer Editor holds original text and shape layers at a small canvas size. |
| mesh-source, mesh-convert | [mesh-convert.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/mesh-convert/1/0b20c5c2539150a2d98ec2f5b405d5610a07afd6bb93150b69a91f4dd89e31a5.ncraft) | An original triangle is kept as GLB. |
| element-reference | [elements.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/elements/1/f0f1b31c67ab7cbf306a09022a9327d698a4c25abefd78de01b7ee5554753ff3.ncraft) | Two images become one named Element reference sheet. |
| storyboard, deck-present, deck-export | [storyboard.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/storyboard/1/b518c83f18442871b538f79c31dae735976e29f06900c066c7e97eaaad268616.ncraft) | Editable Deck; PDF export with 1 or 3 pages |
| generate-document, deck-present, deck-export | [generate-document.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generate-document/1/f32ecaec273d8b2cef58ea9fe729f65e68ede9cd4b0f02ecac3cfeb0bed14e25.ncraft) | Editable Deck; PDF export with 1 or 3 pages |
| image.mask | [action-image-mask.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-mask/1/d5ec0b79ad4ff6f68f7ee428cea0dc48861e91013f52efddc36796f9e2d0fe77.ncraft) | The original image feeds an editable local tool. |
| image.shader | [action-image-shader.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-image-shader/1/6893ab7001ffcb46a37a3ed69f5a1d1d9a686adae49f2e344b78470af4216d08.ncraft) | The original image feeds an editable local tool. |
| mesh.photo | [action-mesh-photo.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-mesh-photo/1/47d7c463f94d539e3da86c79719ce48c4bd01c6fd9f3e2eba2ec3184c6741fac.ncraft) | An original GLB triangle feeds this local 3D capture. |
| mesh.views | [action-mesh-views.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-mesh-views/1/f0fa6128ad9610a40e2da923b5c4d188e94d8e1ac551f036ad8847e234408a85.ncraft) | An original GLB triangle feeds this local 3D capture. |
| mesh.turntable | [action-mesh-turntable.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-mesh-turntable/1/6667e2c35d7e23a814932b1f654683a27d4755a422f2ee8a1edf99bcc565f18e.ncraft) | An original GLB triangle feeds this local 3D capture. |
| video.trim | [action-video-trim.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-trim/1/4e10a1677ebdbc98b1fe6b5aba5a6f45d1f75d09fb15efdd32a03d040a1c9fbe.ncraft) | Trim video edits an original one-second blue clip with a tone. |
| video.speed | [action-video-speed.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-speed/1/e46c108e313f5f26f9a2753dbfa5cb60f4e97665020916980d6255797cc55ae1.ncraft) | Change speed edits an original one-second blue clip with a tone. |
| video.stitch | [action-video-stitch.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-stitch/1/1c790ab73fd5382fc552032faf10b5950ea376d1d15608e5c0fe0e3280db9287.ncraft) | The original rightward-moving clip precedes its reversed clip; the result lasts two seconds. |
| video.replace_audio | [action-video-replace_audio.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-replace_audio/1/b801d6214711995bf1ec4942969cde492ab4d04ec1996a4d78d9f877796bb663.ncraft) | Replace sound edits an original one-second blue clip with a tone. |
| video.frames | [action-video-frames.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-frames/1/61dffca1f2ad1e1bdcc58646d59c64d2b600f950be2db1871bb46ccc70ed1044.ncraft) | Extract frames edits an original one-second blue clip with a tone. |
| video.contact_sheet | [action-video-contact_sheet.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-contact_sheet/1/a5f7094532a17c36c71b0777163451fa3327e3c84a615380ee52accc86f6a5f5.ncraft) | Contact sheet edits an original one-second blue clip with a tone. |
| video.long_exposure | [action-video-long_exposure.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-long_exposure/1/aab7c1b58df0f10cf99ecce17d89d20e08ac223eabb721e1f0166baf5e75df60.ncraft) | Long exposure edits an original one-second blue clip with a tone. |
| video.reverse | [action-video-reverse.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-reverse/1/8ba5d739a04790308f097dfa7de3e9f3db76a8d29e3283bc291add10b0dd0b16.ncraft) | Reverse video edits an original one-second blue clip with a tone. |
| video.ken_burns | [action-video-ken_burns.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-ken_burns/1/d78f7201e733fa8dcefdf25e89384a1acbd6ed7d61254115829fb41825810547.ncraft) | Pan and zoom edits an original one-second blue clip with a tone. |
| video.grade | [action-video-grade.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-grade/1/10bb75acfb71edca7312b657eacaaf9e936a195962e4455da1b935c3052d0e53.ncraft) | Grade video edits an original one-second blue clip with a tone. |
| video.filter | [action-video-filter.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-filter/1/58b31f3af5f53cb850e2eae69ee3d3ac1a829ed9c744c4f4d30ddbd29e3a2b6c.ncraft) | Video effect edits an original one-second blue clip with a tone. |
| video.key | [action-video-key.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-key/1/80c37421d7cc900a8e5ab877570b4d6816104ee12a1eb8afbe4c988b39c46327.ncraft) | Key out a colour edits an original one-second blue clip with a tone. |
| video.watermark | [action-video-watermark.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-watermark/1/efbe926cb56e1ab80d7c15472c41dd73efffbed112159cae220b0c19b042fecd.ncraft) | Watermark video edits an original one-second blue clip with a tone. |
| video.duplicate | [action-video-duplicate.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-duplicate/1/54c572bd43155628b9449dad7f0b14e62fe91465d36a35494a73aaecd8dd96b6.ncraft) | Duplicate video edits an original one-second blue clip with a tone. |
| video.split | [action-video-split.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-split/1/5efeeae55f19ca0ba60f6d0c2fb1dd70aa4e4e15c0187fef0bc2f57f79a61796.ncraft) | Split video edits an original one-second blue clip with a tone. |
| video.extract_audio | [action-video-extract_audio.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/action-video-extract_audio/1/0c19fb1fa1f81f0f1f8172de2ffc159c39b20e658369b1ae8e0c0f8f29f99223.ncraft) | Extract sound edits an original one-second blue clip with a tone. |
| timeline | [timeline.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/timeline/1/3811d2eb5b69703b982172828bd788ce5f4551c344292885a31204eb383146c6.ncraft) | A one-second original clip becomes an automatic Timeline clip. |
| style-training, style-application | [train-style.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/train-style/1/ac6ee3b57c19815d3d25fa2f807207ec3178de4de3192b0a03aebeecbbe316a8.ncraft) | Four references train a reusable style; a compatible image card applies it. |
| folder, folder-filter, folder-range | [folder.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/folder/1/e613ee1ccd3ee7939181aa1456c98ef526bceadf290fc6d729aa69595da68eb0.ncraft) | 2 item results, in source order |
| edit-image | [edit-image.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/edit-image/1/0feb33d77c06bd1951a445a2e3b9aee6f7b4790880d20a7794a63323da50b463.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| image-to-video | [image-to-video.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/image-to-video/1/6f7ad921fa226909b27ba0aa31418280448f81df22c617242a861c99a5f6764c.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| image-to-3d | [image-to-3d.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/image-to-3d/1/0a60e2973415885e1e991f6ba058f826bfd27aca00158359067ad7e045fbb9f6.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| transcribe-audio | [transcribe-audio.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/transcribe-audio/1/850a252c2c189c54fa0b7b60c9abc8a1522d26ad02b1e7bfb2411f87fcefb8ab.ncraft) | Replace the original tiny source and edit the prompt where supported. |
| generation-sheet, variations | [generation-sheet.ncraft](https://github.com/CypherNaught-0x/nodecraft-templates/blob/c5e4f50d61de3e5f4f57b24c8e5c2fb131b5a3d9/templates/generation-sheet/1/74b44b209e531fb3f96ac788679831f38a3e9cc3b6db3aeedec2a8b110a58207.ncraft) | Two Image cards share a sheet; the first makes two variations, the second one (three requests). |

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
temporary output directory. This reconstructs the original seed examples, not a license to replace
published version-1 bytes. New published versions need distinct canvas IDs and immutable paths;
follow the external repository authoring/release instructions, review the graph/index/static-table
diff and run its pinned validator. The author performs no generation, network request or credential
read. No screenshots are committed.

## Updates, offline availability and validation

A fresh native or browser installation embeds the complete pinned set from the data-only
`nodecraft-template-data` Git dependency. It works without contacting GitHub. Builds use an immutable
Git revision and lockfile; there is no build script or build-time HTTP refresh. Normal dependency
prefetch permits offline builds.

**Refresh templates** dispatches `template.refresh {}` and retrieves only the public official index.
The list shows bundled and previously retrieved versions alongside indexed versions. **Retrieve
template** dispatches `template.retrieve {id,version,blake3}` for only that selected archive; it verifies
the exact digest, ordinary graph, embedded assets and node kinds before making **Add template**
available. Retrieval never adds a canvas automatically. A new index advertising v2 leaves usable
bundled v1 available when v2 has not been retrieved or the repository is unreachable. Unknown schemas,
invalid content or failed updates retain the last good catalog and all bundled examples.

Downloads use a bounded in-memory cache (64 MiB, at most 128 archives) for the current project
Session on both platforms. They are not a persistent personal library: closing/replacing the Session
removes them. Once added, a template's graph and assets are ordinary project data and survive saving
and reloading without that cache. One request runs at a time; each index is at most 128 KiB and each
archive at most 2 MiB. Decode/import temporaries are separately bounded by those stored-archive
limits. Requests use a fixed public origin without provider credentials; native requests refuse
redirects, while browser Fetch owns its redirect policy. Late results from a replaced Session are
discarded. Neither refresh nor retrieval changes project history or starts a paid request.

The external repository CI pins an exact NodeCraft validator source commit and invokes the same
production parser, digest, archive/asset and kind validation used by the client. Its
`template_catalog` test target proves rejection of a digest mismatch and an invalid archive even
when its digest is correct. A bounded append-only `history.json` records every published version, including those absent from
the current index; runtime refresh never downloads this authoring ledger. CI compares the entire
previous ledger, requires its exact archive inventory, verifies every historical archive and refuses
all root/auxiliary canvas-ID reuse across versions, deletion, replacement and version rollback. See the external README for its exact validator command and release steps.
