//! The ComfyUI server's own models and the document's imported workflows as model choices
//! (#970; rules in docs/decisions/2026-11-03-comfy-server-models-in-picker.md).
//!
//! Nothing here fetches: the lists are read from the default server's node list that
//! `comfy.nodes.refresh` installed. `comfy.models.list` groups what the server reports
//! (checkpoints, LoRAs, VAEs, text encoders, diffusion models, upscalers) and
//! says, for each entry, which built-in workflow runs it or why none can. `comfy.models.preview`
//! and `comfy.models.use` put a checkpoint on existing cards through the shared model swap, with
//! the checkpoint set in the same undo step; `comfy.models.add` places a new card for a checkpoint
//! or a copy of an imported workflow card. A name the server doesn't list is refused, never
//! replaced by another.
use crate::comfy_workflow_cmds::NodeCatalog;
use crate::commands::{CommandSpec, Context, Effect, Phase, Resources, invalid, params};
use crate::generation_catalog::GenerationCatalog;
use crate::model_swap::{self, Preview, Selection};
use nodecraft_graph::{Change, Changeset, MAX_CHANGES, Node, Position};
use nodecraft_nodes::{comfy_workflow, generation};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ParamValues, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

/// The group of checkpoints, the only one the built-in workflows run.
pub const CHECKPOINTS: &str = "checkpoints";
/// The card parameter a checkpoint goes into.
pub const CHECKPOINT_PARAM: &str = "checkpoint";
/// The longest name the card's `checkpoint` field holds (its catalog `max_bytes`).
pub const MAX_NAME_BYTES: usize = 512;
/// At most this many server entries are listed in all.
pub const MAX_ENTRIES: usize = 2048;
/// At most this many entries are listed in one group, within [`MAX_ENTRIES`].
pub const MAX_GROUP_ENTRIES: usize = 512;
/// At most this many bytes of names are listed in all.
const MAX_NAME_TOTAL: usize = 512 * 1024;
/// At most this many imported workflows are listed.
pub const MAX_WORKFLOWS: usize = 256;
/// The canvas extent positions may use (as `node.create`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// The longest name a request may carry; longer is refused before any lookup.
const MAX_REQUEST_NAME: usize = 4096;

/// One server list: the core loader whose combo input lists it, and why its entries can't run
/// on their own when they can't.
struct Source {
    id: &'static str,
    label: &'static str,
    modality: &'static str,
    class: &'static str,
    input: &'static str,
    unavailable: Option<&'static str>,
}

const SOURCES: &[Source] = &[
    Source { id: CHECKPOINTS, label: "Checkpoints", modality: "image", class: "CheckpointLoaderSimple", input: "ckpt_name", unavailable: None },
    Source {
        id: "loras",
        label: "LoRAs",
        modality: "image",
        class: "LoraLoader",
        input: "lora_name",
        unavailable: Some(
            "A LoRA changes a base model and can't run on its own, and the built-in workflows load none. Import a workflow that applies it; it then appears under Workflows.",
        ),
    },
    Source {
        id: "vaes",
        label: "VAEs",
        modality: "image",
        class: "VAELoader",
        input: "vae_name",
        unavailable: Some("A VAE is part of a model, not one to run; the built-in workflows use the checkpoint's own. Import a workflow that loads it."),
    },
    Source {
        id: "text_encoders",
        label: "Text encoders",
        modality: "image",
        class: "CLIPLoader",
        input: "clip_name",
        unavailable: Some(
            "A text encoder is part of a model, not one to run; the built-in workflows use the checkpoint's own. Import a workflow that loads it.",
        ),
    },
    Source {
        id: "diffusion_models",
        label: "Diffusion models",
        modality: "image",
        class: "UNETLoader",
        input: "unet_name",
        unavailable: Some(
            "A diffusion model needs its text encoders and VAE chosen with it, which the built-in workflows don't do. Import a workflow that loads it.",
        ),
    },
    Source {
        id: "upscalers",
        label: "Upscalers",
        modality: "image",
        class: "UpscaleModelLoader",
        input: "model_name",
        unavailable: Some("NodeCraft has no built-in upscaling workflow. Import a workflow that uses this upscaler."),
    },
];

fn source(id: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|source| source.id == id)
}

const VIDEO_CHECKPOINT: &str = "This looks like a video checkpoint, and the built-in workflows make images. Import a workflow that uses it.";
const AUDIO_CHECKPOINT: &str = "This looks like an audio checkpoint, and the built-in workflows make images. Import a workflow that uses it.";
const SD3_CHECKPOINT: &str = "SD3 checkpoints need their own workflow, which NodeCraft doesn't have built in. Import a workflow that uses it.";
const OTHER_ARCHITECTURE: &str =
    "This looks like an inpainting or refiner checkpoint, or a model family the built-in workflows don't run. Import a workflow that uses it.";
const FLUX_TOOL: &str = "This FLUX tool model needs an image or mask input the built-in FLUX workflow doesn't give it. Import a workflow that uses it.";
const TOO_LONG: &str = "This name is longer than a card's checkpoint field holds (512 bytes).";
const NO_WORKFLOW: &str = "This NodeCraft's catalog has no built-in ComfyUI image workflow to run it.";
/// The warning on a checkpoint that looks like SD 1.x or 2.x.
pub const SD12_NOTE: &str =
    "Looks like SD 1.x or 2.x: it runs through the SDXL graph, which renders at about 1 megapixel while SD 1.x expects 512 px, so results may be garbled.";
/// The warning on a checkpoint whose architecture the name doesn't tell.
pub const UNKNOWN_NOTE: &str = "Architecture not recognised: it runs through the SDXL graph and may fail.";

/// How a checkpoint runs: the catalog model of its built-in workflow, and a warning to show with
/// it when the fit is a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Route {
    model: &'static str,
    note: Option<&'static str>,
}

/// Whether `word` starts a word in `lower`: at its start, or after a character that isn't a
/// letter or digit.
fn word_start(lower: &str, word: &str) -> bool {
    lower.match_indices(word).any(|(at, _)| lower.get(..at).and_then(|before| before.chars().next_back()).is_none_or(|c| !c.is_alphanumeric()))
}

/// The built-in workflow a checkpoint runs through, or why none fits. A guess from the file name
/// only, in this order (docs/decisions/2026-11-03-comfy-server-models-in-picker.md): video, audio,
/// SD3 and other architectures are unavailable; FLUX names go to the FLUX graph unless they are a
/// tool model; SDXL-family names go to the SDXL graph; SD 1.x/2.x names and names it can't tell
/// go there too, with a warning.
fn checkpoint_route(lower: &str) -> Result<Route, &'static str> {
    let tokens: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let token = |wanted: &[&str]| tokens.iter().any(|t| wanted.contains(t));
    let versioned = |prefix: &str| tokens.iter().any(|t| t.strip_prefix(prefix).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())));
    let starts = |words: &[&str]| words.iter().any(|word| word_start(lower, word));
    if token(&["svd", "ltx", "ltxv", "mochi", "cosmos", "t2v", "i2v", "video"])
        || versioned("wan")
        || versioned("ltxv")
        || lower.contains("hunyuan_video")
        || lower.contains("hunyuanvideo")
    {
        return Err(VIDEO_CHECKPOINT);
    }
    if token(&["audio"]) || lower.contains("ace_step") {
        return Err(AUDIO_CHECKPOINT);
    }
    let sd_then_3 = tokens.windows(2).any(|pair| matches!(pair, [first, second] if *first == "sd" && second.starts_with('3')));
    if versioned("sd3") || sd_then_3 || lower.contains("stablediffusion3") || lower.contains("stable_diffusion_3") {
        return Err(SD3_CHECKPOINT);
    }
    if starts(&["inpaint", "refiner", "pix2pix", "cascade", "pixart", "kolors", "auraflow", "hidream", "lumina", "chroma", "qwen", "hunyuan"]) {
        return Err(OTHER_ARCHITECTURE);
    }
    if word_start(lower, "flux") {
        if starts(&["fill", "kontext", "redux", "depth", "canny"]) {
            return Err(FLUX_TOOL);
        }
        return Ok(Route { model: "comfyui.flux-dev", note: None });
    }
    if ["sd15", "sd1.5", "sd_1.5", "v1-5", "v1_5", "sd21", "sd2.1", "sd_2.1", "v2-1", "768-v"].iter().any(|hint| lower.contains(hint)) {
        return Ok(Route { model: "comfyui.sdxl", note: Some(SD12_NOTE) });
    }
    if tokens.iter().any(|t| t.ends_with("xl")) || starts(&["sdxl", "pony", "illustrious", "noob", "animagine"]) {
        return Ok(Route { model: "comfyui.sdxl", note: None });
    }
    Ok(Route { model: "comfyui.sdxl", note: Some(UNKNOWN_NOTE) })
}

/// One model the server lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// The catalog model whose built-in workflow runs it, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Why this entry can't run, when its group as a whole could.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// A warning to show with a runnable entry whose fit is a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One type of model the server lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub label: String,
    pub modality: String,
    /// Why none of this group's entries can run, when that holds for all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    pub models: Vec<Entry>,
    /// Entries left out past the listing's limits.
    pub more: usize,
    /// Empty names, names with control, bidirectional or zero-width characters, and repeats.
    pub skipped: usize,
}

/// Format characters that could make a listed name read as another (bidirectional overrides,
/// zero-width characters, the soft hyphen, the byte-order mark).
fn invisible(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}')
}

fn usable(name: &str) -> bool {
    !name.trim().is_empty() && !name.chars().any(|c| c.is_control() || invisible(c))
}

/// What the server's node list says it has, grouped, with how each entry runs (if it does).
/// `catalog` decides whether a mapped built-in workflow exists for image cards.
pub fn inventory(nodes: &NodeCatalog, catalog: Option<&GenerationCatalog>) -> Vec<Group> {
    let mut entries = 0usize;
    let mut bytes = 0usize;
    let mut groups = Vec::new();
    for source in SOURCES {
        let Some(listed) = nodes.options(source.class, source.input) else { continue };
        let mut group = Group {
            id: source.id.into(),
            label: source.label.into(),
            modality: source.modality.into(),
            unavailable: source.unavailable.map(str::to_owned),
            models: Vec::new(),
            more: 0,
            skipped: 0,
        };
        let mut seen = BTreeSet::new();
        for name in listed {
            if !usable(name) || !seen.insert(name.as_str()) {
                group.skipped = group.skipped.saturating_add(1);
                continue;
            }
            let shown = name.len().min(MAX_NAME_BYTES.saturating_add(1));
            if entries >= MAX_ENTRIES || group.models.len() >= MAX_GROUP_ENTRIES || bytes.saturating_add(shown) > MAX_NAME_TOTAL {
                group.more = group.more.saturating_add(1);
                continue;
            }
            let entry = if name.len() > MAX_NAME_BYTES {
                // Listed by its first characters only, so a hostile name can't fill the list.
                let mut end = MAX_NAME_BYTES;
                while !name.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                Entry { name: format!("{}…", name.get(..end).unwrap_or_default()), model: None, unavailable: Some(TOO_LONG.into()), note: None }
            } else if source.unavailable.is_some() {
                Entry { name: name.clone(), model: None, unavailable: None, note: None }
            } else {
                match checkpoint_route(&name.to_lowercase()) {
                    Ok(route) if catalog.is_none_or(|catalog| catalog.model(generation::KIND, route.model).is_some()) => {
                        Entry { name: name.clone(), model: Some(route.model.into()), unavailable: None, note: route.note.map(str::to_owned) }
                    }
                    Ok(_) => Entry { name: name.clone(), model: None, unavailable: Some(NO_WORKFLOW.into()), note: None },
                    Err(reason) => Entry { name: name.clone(), model: None, unavailable: Some(reason.into()), note: None },
                }
            };
            entries = entries.saturating_add(1);
            bytes = bytes.saturating_add(shown);
            group.models.push(entry);
        }
        groups.push(group);
    }
    groups
}

/// The server name whose node list is installed, when it was read through a named server (#864).
pub(crate) struct ListedServer(pub(crate) String);

fn installed(resources: &Resources) -> Result<&NodeCatalog, Error> {
    crate::comfy_workflow_cmds::installed(resources).ok_or_else(|| {
        Error::new(
            ErrorKind::Unsupported,
            "NodeCraft hasn't read the default ComfyUI server's models yet; refresh them (comfy.nodes.refresh), then choose again",
        )
    })
}

/// The catalog model the server entry `group`/`name` runs through. Refused, never substituted,
/// when the server doesn't list it or no built-in workflow runs it.
fn resolve(c: &Context<'_>, group: &str, name: &str) -> Result<&'static str, Error> {
    let source = source(group).ok_or_else(|| invalid("unknown ComfyUI model group; see comfy.models.list for the groups"))?;
    if name.len() > MAX_REQUEST_NAME {
        return Err(Error::new(ErrorKind::Overflow, "the model name is longer than 4096 bytes"));
    }
    let nodes = installed(c.resources)?;
    let listed = nodes.options(source.class, source.input).unwrap_or_default();
    if !usable(name) || !listed.iter().any(|listed| listed == name) {
        return Err(Error::new(
            ErrorKind::NotFound,
            "the default ComfyUI server doesn't list that model, so nothing was changed or substituted. Install it on the server and refresh, or choose one it lists",
        ));
    }
    if let Some(reason) = source.unavailable {
        return Err(Error::new(ErrorKind::Unsupported, reason));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(Error::new(ErrorKind::Unsupported, TOO_LONG));
    }
    let model = checkpoint_route(&name.to_lowercase()).map_err(|reason| Error::new(ErrorKind::Unsupported, reason))?.model;
    let catalog = GenerationCatalog::current(c.resources)?;
    if catalog.model(generation::KIND, model).is_none() {
        return Err(Error::new(ErrorKind::Unsupported, NO_WORKFLOW));
    }
    Ok(model)
}

/// One imported workflow card, as a choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub canvas: CanvasId,
    pub id: NodeId,
    pub title: String,
    /// The media its results are (its output ports).
    pub outputs: Vec<String>,
    /// The media kinds its saved outputs make (`image`, `video`, `audio`, `mesh`), from its mapping.
    #[serde(default)]
    pub modalities: Vec<String>,
    /// Why it can't be copied, when it can't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// The document's imported workflow cards, at most [`MAX_WORKFLOWS`], and how many more.
fn workflows(c: &Context<'_>) -> (Vec<Workflow>, usize) {
    let mut listed = Vec::new();
    let mut more = 0usize;
    for (canvas_id, canvas) in c.document.canvases() {
        for node in canvas.nodes().values().filter(|node| node.kind == comfy_workflow::ID) {
            if listed.len() >= MAX_WORKFLOWS {
                more = more.saturating_add(1);
                continue;
            }
            let title = if node.title.trim().is_empty() { "ComfyUI workflow".to_owned() } else { bounded(&node.title, 256) };
            let unavailable = comfy_workflow::card(node).err().map(|_| "This workflow card can't be read (it may come from a newer NodeCraft).".to_owned());
            let outputs = node.outputs.iter().take(16).map(|port| bounded(&port.name, 64)).collect();
            let mut modalities: Vec<String> = comfy_workflow::mapping(node)
                .map(|mapping| mapping.outputs.iter().filter_map(|output| serde_json::to_value(output.modality).ok()?.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            modalities.sort();
            modalities.dedup();
            listed.push(Workflow { canvas: *canvas_id, id: node.id, title, outputs, modalities, unavailable });
        }
    }
    (listed, more)
}

fn bounded(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    #[serde(default)]
    canvas: Option<CanvasId>,
    #[serde(default)]
    nodes: Option<Vec<NodeId>>,
}

fn check_list(value: &Json) -> Result<ListParams, Error> {
    let p: ListParams = params(value)?;
    if p.nodes.is_some() != p.canvas.is_some() {
        return Err(invalid("name both canvas and nodes, or neither"));
    }
    if p.nodes.as_ref().is_some_and(|nodes| nodes.len() > MAX_CHANGES) {
        return Err(Error::new(ErrorKind::Overflow, "name at most 10,000 cards"));
    }
    Ok(p)
}

fn enabled_list(_: &Context<'_>, value: &Json) -> Result<(), Error> {
    check_list(value).map(|_| ())
}

/// Each target's current checkpoint, when it runs a built-in ComfyUI workflow, and whether the
/// server lists it (`null` before the server's models were read).
fn current(c: &Context<'_>, canvas: CanvasId, ids: &[NodeId], nodes: Option<&NodeCatalog>) -> Result<Vec<Json>, Error> {
    let canvas = c.document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let catalog = GenerationCatalog::current(c.resources).ok();
    let listed = nodes.and_then(|nodes| nodes.options("CheckpointLoaderSimple", "ckpt_name"));
    let mut current = Vec::new();
    for id in ids {
        let node = canvas.nodes().get(id).ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
        if !catalog.is_some_and(|catalog| crate::comfy_server_cmds::runs_on_comfy(catalog, node)) {
            continue;
        }
        let Some(Value::Text(checkpoint)) = node.params.get(CHECKPOINT_PARAM) else { continue };
        let present = listed.map(|listed| listed.iter().any(|name| name == checkpoint));
        current.push(json!({"id": id, "checkpoint": bounded(checkpoint, MAX_NAME_BYTES), "present": present}));
    }
    Ok(current)
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p = check_list(value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Json::Null });
    }
    let nodes = crate::comfy_workflow_cmds::installed(c.resources);
    let catalog = GenerationCatalog::current(c.resources).ok();
    let groups = nodes.map(|nodes| inventory(nodes, catalog)).unwrap_or_default();
    let (workflows, more_workflows) = workflows(c);
    let current = match (p.canvas, &p.nodes) {
        (Some(canvas), Some(ids)) => current(c, canvas, ids, nodes)?,
        _ => Vec::new(),
    };
    let server = c.resources.get::<ListedServer>().filter(|_| nodes.is_some()).map(|server| server.0.clone());
    let result = json!({
        "read": nodes.is_some(),
        "server": server,
        "groups": groups,
        "workflows": workflows,
        "more_workflows": more_workflows,
        "current": current,
    });
    crate::commands::validate_json(&result)?;
    Ok(Effect::None { result })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UseParams {
    canvas: CanvasId,
    nodes: Vec<NodeId>,
    group: String,
    name: String,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    preview_binding: Option<String>,
}

struct Prepared {
    preview: Preview,
    model: &'static str,
    changes: Changeset,
}

/// The swap's binding, extended with the server entry and which read of which default server
/// listed it, so a preview made before a refresh or a change of default never applies after it.
fn binding(c: &Context<'_>, swap: &str, group: &str, name: &str) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.comfy-models.v1");
    hash.update(swap.as_bytes());
    let server = c.resources.get::<ListedServer>().map_or("", |server| server.0.as_str());
    match crate::comfy_workflow_cmds::last_id(c.resources) {
        Some(id) => hash.update(&[1]).update(&id.to_le_bytes()),
        None => hash.update(&[0]),
    };
    for part in [server, group, name] {
        hash.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

/// The shared model swap to the entry's built-in workflow, with its checkpoint set on every
/// target in the same changeset.
fn prepare(c: &Context<'_>, p: &UseParams) -> Result<Prepared, Error> {
    let model = resolve(c, &p.group, &p.name)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    if p.nodes.iter().any(|id| canvas.nodes().get(id).is_some_and(|node| node.kind != generation::KIND)) {
        return Err(Error::new(ErrorKind::Unsupported, "a ComfyUI server checkpoint runs only on Generate image cards"));
    }
    let selection = Selection { canvas: p.canvas, nodes: p.nodes.clone(), model: model.into() };
    let swap = model_swap::prepare(c, &selection)?;
    let catalog = GenerationCatalog::current(c.resources)?;
    let mut proposed: BTreeMap<NodeId, Node> = swap
        .changes
        .changes
        .into_iter()
        .filter_map(|change| match change {
            Change::SetNode { node, .. } => Some((node.id, node)),
            _ => None,
        })
        .collect();
    let mut preview = swap.preview;
    let mut changes = Vec::new();
    for target in &mut preview.targets {
        let existing = canvas.nodes().get(&target.id).ok_or_else(|| Error::new(ErrorKind::NotFound, "selected node does not exist"))?;
        let base = proposed.remove(&target.id).unwrap_or_else(|| existing.clone());
        let mut values: ParamValues = base
            .params
            .iter()
            .filter(|(name, _)| name.as_str() != generation::MODEL && name.as_str() != generation::PROMPT)
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        values.insert(CHECKPOINT_PARAM.into(), Value::Text(p.name.clone()));
        let candidate = catalog.prepare_instance(existing, model, &values)?;
        catalog.validate_wiring(canvas, c.kinds, &candidate, false)?;
        target.kept.retain(|name| name != CHECKPOINT_PARAM);
        target.kept.push(CHECKPOINT_PARAM.into());
        target.defaults.retain(|name| name != CHECKPOINT_PARAM);
        target.reset.retain(|name| name != CHECKPOINT_PARAM);
        target.dropped.retain(|dropped| dropped.param != CHECKPOINT_PARAM);
        target.changed = candidate != *existing;
        if target.changed {
            changes.push(Change::SetNode { canvas: p.canvas, node: candidate });
        }
    }
    preview.binding = binding(c, &preview.binding, &p.group, &p.name);
    let changes = Changeset::new(changes);
    changes.validate_replay_budget(crate::session::MAX_HISTORY_BYTES.saturating_sub(12))?;
    Ok(Prepared { preview, model, changes })
}

fn preview_result(prepared: &Prepared, p: &UseParams) -> Result<Json, Error> {
    let mut result = serde_json::to_value(&prepared.preview).map_err(|_| invalid("model preview could not be encoded"))?;
    if let Some(object) = result.as_object_mut() {
        object.insert("server_model".into(), json!({"group": p.group, "name": p.name, "param": CHECKPOINT_PARAM, "workflow": prepared.model}));
    }
    crate::commands::validate_json(&result)?;
    Ok(result)
}

fn preview_params(value: &Json) -> Result<UseParams, Error> {
    let p: UseParams = params(value)?;
    if p.expected_revision.is_some() || p.preview_binding.is_some() {
        return Err(invalid("comfy.models.preview takes no expected_revision or preview_binding"));
    }
    Ok(p)
}

fn enabled_preview(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    let p = preview_params(value)?;
    prepare(c, &p).map(|_| ())
}

fn preview(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p = preview_params(value)?;
    let prepared = prepare(c, &p)?;
    Ok(Effect::None { result: preview_result(&prepared, &p)? })
}

/// The reviewed preview, checked to be the one this request names.
fn checked(c: &Context<'_>, value: &Json) -> Result<(UseParams, Prepared), Error> {
    c.editable()?;
    let p: UseParams = params(value)?;
    let (Some(revision), Some(token)) = (p.expected_revision, p.preview_binding.as_deref()) else {
        return Err(invalid("expected_revision and preview_binding are required: take them from comfy.models.preview"));
    };
    let prepared = prepare(c, &p)?;
    if revision != c.revision || token.len() != 64 || token != prepared.preview.binding {
        return Err(Error::new(ErrorKind::Conflict, "model preview is stale; refresh and review the complete proposal"));
    }
    Ok((p, prepared))
}

fn enabled_use(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    checked(c, value).map(|_| ())
}

fn use_model(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let (p, prepared) = checked(c, value)?;
    let result = preview_result(&prepared, &p)?;
    if prepared.changes.changes.is_empty() {
        return Ok(Effect::None { result });
    }
    Ok(Effect::Edit { changes: prepared.changes, result, gesture: None })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowRef {
    canvas: CanvasId,
    id: NodeId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    canvas: CanvasId,
    id: NodeId,
    pos: Position,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    workflow: Option<WorkflowRef>,
}

/// A copy of an imported workflow card: its workflow, mapping and values, without its results
/// or server choice (the copy follows the default server).
fn workflow_copy(c: &Context<'_>, from: &WorkflowRef, id: NodeId) -> Result<Node, Error> {
    let canvas = c.document.canvases().get(&from.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&from.id).ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    if source.kind != comfy_workflow::ID {
        return Err(invalid("that card isn't an imported ComfyUI workflow"));
    }
    comfy_workflow::card(source)?;
    let mut config = source.config.clone();
    config.remove(crate::provider_setup::COMFY_SERVER_CONFIG);
    let values: ParamValues = source.params.iter().filter(|(name, _)| name.as_str() != comfy_workflow::MODEL).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut node = c.kinds.get(comfy_workflow::ID)?.instantiate_instance(id, config, &values)?;
    node.title.clone_from(&source.title);
    Ok(node)
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let (mut node, result) = match (&p.group, &p.name, &p.workflow) {
        (Some(group), Some(name), None) => {
            let model = resolve(c, group, name)?;
            let catalog = GenerationCatalog::current(c.resources)?;
            let base = crate::create_cmds::instance(c.kinds, generation::KIND, p.id)?;
            let values: ParamValues = [(CHECKPOINT_PARAM.into(), Value::Text(name.clone()))].into();
            let node = catalog.prepare_instance(&base, model, &values)?;
            (node, json!({"id": p.id, "model": model, "checkpoint": name}))
        }
        (None, None, Some(workflow)) => {
            let node = workflow_copy(c, workflow, p.id)?;
            let result = json!({"id": p.id, "workflow": {"canvas": workflow.canvas, "id": workflow.id}});
            (node, result)
        }
        _ => return Err(invalid("name a server model with group and name, or an imported workflow card with workflow")),
    };
    node.pos = p.pos;
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

fn enabled_add(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    c.editable()?;
    add(c, value).map(|_| ())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn nodes_schema() -> Json {
    json!({"type": "array", "minItems": 1, "maxItems": MAX_CHANGES, "uniqueItems": true, "items": uuid()})
}

fn group_schema() -> Json {
    json!({"type": "string", "enum": SOURCES.iter().map(|source| source.id).collect::<Vec<_>>()})
}

fn name_schema() -> Json {
    json!({"type": "string", "minLength": 1, "maxLength": MAX_REQUEST_NAME})
}

fn list_schema() -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [],
        "properties": {"canvas": uuid(), "nodes": nodes_schema()}})
}

fn use_schema(commit: bool) -> Json {
    let mut schema = json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "nodes", "group", "name"],
        "properties": {"canvas": uuid(), "nodes": nodes_schema(), "group": group_schema(), "name": name_schema()}});
    if commit {
        if let Some(required) = schema.get_mut("required").and_then(Json::as_array_mut) {
            required.extend([json!("expected_revision"), json!("preview_binding")]);
        }
        if let Some(properties) = schema.get_mut("properties").and_then(Json::as_object_mut) {
            properties.insert("expected_revision".into(), json!({"type": "integer", "minimum": 0}));
            properties.insert("preview_binding".into(), json!({"type": "string", "minLength": 64, "maxLength": 64}));
        }
    }
    schema
}

fn preview_schema() -> Json {
    use_schema(false)
}

fn commit_schema() -> Json {
    use_schema(true)
}

fn add_schema() -> Json {
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "pos"],
        "properties": {"canvas": uuid(), "id": uuid(),
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "group": group_schema(), "name": name_schema(),
            "workflow": {"type": "object", "required": ["canvas", "id"], "additionalProperties": false, "properties": {"canvas": uuid(), "id": uuid()}}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "comfy.models.list",
            label: "List ComfyUI server models",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas?, nodes?: NodeId[]} -> {read, server, groups: [{id, label, modality, unavailable?, models: [{name, model?, unavailable?, note?}], more, skipped}], workflows: [{canvas, id, title, outputs, modalities, unavailable?}], more_workflows, current: [{id, checkpoint, present}]}; what the default ComfyUI server's last node list (comfy.nodes.refresh) says it has, grouped (checkpoints, loras, vaes, text_encoders, diffusion_models, upscalers), each entry with the catalog model whose built-in workflow runs it (and a warning when that is a guess) or why none can (a group-wide reason sits on the group), and the document's imported workflow cards. `read` is false and groups empty before the server was read. With canvas and nodes, `current` says whether each ComfyUI card's checkpoint is on the server (null before it was read). Read-only.",
            enabled: enabled_list,
            run: list,
        },
        CommandSpec {
            id: "comfy.models.preview",
            label: "Preview ComfyUI server model",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: preview_schema,
            params_doc: "{canvas, nodes: NodeId[], group, name} -> the node.model_preview result for the entry's built-in workflow, with the checkpoint kept, plus server_model: {group, name, param, workflow}; read-only. Refused, never substituted, when the server's models haven't been read, the server doesn't list the name, or the entry can't run (comfy.models.list says why). No run or payment consent.",
            enabled: enabled_preview,
            run: preview,
        },
        CommandSpec {
            id: "comfy.models.use",
            label: "Use ComfyUI server model",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: commit_schema,
            params_doc: "{canvas, nodes: NodeId[], group, name, expected_revision, preview_binding} -> the preview; applies exactly the reviewed comfy.models.preview: the cards change to the built-in ComfyUI workflow and run that checkpoint, one Undo step. Refused when stale or when the server no longer lists the name. No run or payment consent.",
            enabled: enabled_use,
            run: use_model,
        },
        CommandSpec {
            id: "comfy.models.add",
            label: "Add card from ComfyUI server",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{canvas, id, pos: {x, y}, group?, name?, workflow?: {canvas, id}} -> {id, …}; with group and name, a new Generate image card running that server checkpoint through its built-in workflow (refused, never substituted, when the server doesn't list it or it can't run); with workflow, a copy of that imported workflow card (same workflow, mapping and values; no results; follows the default server). The caller supplies a new id. One Undo step.",
            enabled: enabled_add,
            run: add,
        },
    ]
}

#[cfg(test)]
mod tests;
