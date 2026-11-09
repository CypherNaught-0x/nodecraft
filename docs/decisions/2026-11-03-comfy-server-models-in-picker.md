## 2026-11-03 · The model picker lists the ComfyUI server's own models and imported workflows (#970)

Until now the model picker read only catalog entries, so a ComfyUI server offered just the two
built-in cards (FLUX.1 [dev] and SDXL). The picker now also lists what the **default server's**
last node list (`comfy.nodes.refresh`, #9/#864) says it has. The rules below decide which of
those entries a card can run, and how.

**Where the list comes from.** The engine reads the combo inputs of core loader nodes in the
installed `/object_info`; it fetches nothing itself. The command `comfy.models.list` returns the
groups in a fixed order:

| Group | Read from | Modality |
|---|---|---|
| Checkpoints | `CheckpointLoaderSimple.ckpt_name` | image |
| LoRAs | `LoraLoader.lora_name` | image |
| VAEs | `VAELoader.vae_name` | image |
| Text encoders | `CLIPLoader.clip_name` | image |
| Diffusion models | `UNETLoader.unet_name` | image |
| Upscalers | `UpscaleModelLoader.model_name` | image |

`ImageOnlyCheckpointLoader` lists the same checkpoints folder, so it isn't read: its entries
would appear twice. Video and audio checkpoints show up among the checkpoints and are marked by
the name hints below. A group the server's node list doesn't describe is left out.

The node list follows the default server, so changing or removing the default empties these
groups until the new default is read. Only the default's list is kept (#864). The result names
that server, and the picker heads the section **From ComfyUI server "name"**, so a person can
tell it apart from catalog entries. If the list itself can't be read, the heading row says why.

**Which cards.** Only Generate image cards (`generation.image`) get the server section, and only
they can take a server checkpoint. `comfy.models.preview`/`use` refuse any other kind. Among the
imported workflows, the picker offers an image card only those whose saved outputs include images.

**How a checkpoint runs.** A checkpoint runs through a built-in image workflow, with its name put
into the card's `checkpoint` parameter. The engine guesses which from the lowercased file name.
It splits the name into *words* at every character that isn't a letter or digit; a word *starts*
at the beginning of the name or after such a character. The rules apply in this order, and the
first that matches decides:

1. **Video: unavailable.** The words `svd`, `ltx`, `ltxv`, `mochi`, `cosmos`, `t2v`, `i2v` or
   `video`; `wan` or `ltxv` followed only by digits; or `hunyuan_video` or `hunyuanvideo`
   anywhere.
2. **Audio: unavailable.** The word `audio`, or `ace_step` anywhere.
3. **SD3: unavailable.** `sd3` followed only by digits; the word `sd` followed by a word starting
   with `3`; or `stablediffusion3` or `stable_diffusion_3` anywhere.
4. **Other architectures: unavailable.** A word starting `inpaint`, `refiner`, `pix2pix`,
   `cascade`, `pixart`, `kolors`, `auraflow`, `hidream`, `lumina`, `chroma`, `qwen` or `hunyuan`.
5. **FLUX.** A word starting `flux` (so `InfluxRealism` doesn't count) maps to
   `comfyui.flux-dev`. It is unavailable instead when a word starts `fill`, `kontext`, `redux`,
   `depth` or `canny`: those tool models need an image or mask input the built-in FLUX workflow
   doesn't give them.
6. **SD 1.x/2.x.** `sd15`, `sd1.5`, `sd_1.5`, `v1-5`, `v1_5`, `sd21`, `sd2.1`, `sd_2.1`, `v2-1` or
   `768-v` anywhere map to `comfyui.sdxl`, with a warning. The SDXL graph renders at about one
   megapixel while SD 1.x expects 512 px, so results may be garbled.
7. **SDXL family.** A word ending `xl`, or a word starting `sdxl`, `pony`, `illustrious`, `noob`
   or `animagine`, maps to `comfyui.sdxl`.
8. **Anything else** maps to `comfyui.sdxl`, with the warning "Architecture not recognised: it
   runs through the SDXL graph and may fail."

Unavailable entries can still be typed into a card by hand. The table lives in the engine
(`comfy_models`), not in each surface; the UI, MCP and CLI read the result as data.

**What else is listed as unavailable.** These entries stay visible, each with its reason, so
nothing a server reports is hidden:

- **Parts of a model.** A LoRA, VAE or text encoder can't run on its own. A diffusion model
  (UNet) needs its text encoders and VAE chosen with it. The built-in workflows don't load these
  separately. The reason says to import a workflow that uses them (#9); that workflow then shows
  up in the picker's Workflows group. A loader added to the built-in graph can come later. It
  needs new card parameters and its own option sources, so it isn't part of this change.
- **Upscalers.** NodeCraft has no built-in upscaling workflow.
- **Too long.** A name longer than the card's `checkpoint` field (512 bytes) is listed, cut short,
  as too long.

**Skipped names and limits.** Names that are empty or contain control, bidirectional-override or
zero-width characters are skipped and counted, and so are repeats. Those names can't be chosen
through the commands either. The list holds at most 512 entries a group, 2,048 in all, and
512 KiB of names. Each group says how many it left out (`more`).

**Imported workflows.** Every `generation.comfy_workflow` card in the document is a choice under
its title, carrying its saved mapping and the media its outputs make. A workflow card is its own
kind, so choosing one can't change an image card's model. Instead `comfy.models.add {workflow}`
places a copy of that workflow card beside the card, with the same workflow, mapping and
parameter values. The copy has no results and no server choice: it follows the default server.
The original card is unchanged.

**Choosing, and refusing a missing model.** On an existing card the choice goes through
`comfy.models.preview` and then `comfy.models.use`. That is the same source-aware model swap as
`node.model_preview`/`node.set_model`, with the checkpoint set in the same single undo step. The
binding covers the server entry's group and name, the default server's name and the id of its
last read. A preview made before a refresh or a change of default therefore never applies after
it. `comfy.models.add {group, name}` places a new image card already set to that checkpoint. All
three commands refuse, and never substitute, when:

- the server's nodes haven't been read;
- the name isn't in the installed list;
- the entry is unavailable.

Their errors don't repeat the requested name: the engine's command fuzzing (`panic_hunt`)
requires that no error echoes an input value.

At run time the built-in workflow already refuses a checkpoint the server doesn't list, before
anything uploads (#9). When `comfy.models.list` gets `{canvas, nodes}`, it also reports each
target's current checkpoint as present or **missing**. The picker shows a missing one as missing
and never swaps it for another.

**Refresh and keys.** The picker's **Refresh server models** dispatches `comfy.nodes.refresh` for
the default server. The open picker rebuilds its server section, keeping the search text, when
the installed node list changes, whether through a refresh or through a change of default.
The arrow keys and Enter reach server entries and workflows as well as catalog models.
