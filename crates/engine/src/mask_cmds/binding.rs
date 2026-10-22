//! Masks bound to the image they were painted over (#546). `node.mask_set` records the image's
//! content hash and size with the first strokes; when the image changes the mask is stale, and
//! when its shape changes a run that would use the mask is blocked until `mask.refit` keeps the
//! strokes as they are or fits them onto the new image. Masks saved before this record nothing
//! and are never stale.
use super::{CONFIG, EXPAND, ID, Margins, Painted, Stroke, Strokes, margins, not_found, strokes};
use crate::commands::{CommandSpec, Context, Effect, Phase, invalid, params};
use nodecraft_actions::mask::MIN_SIZE;
use nodecraft_graph::{Canvas, Change, Changeset, Node};
use nodecraft_types::{AssetRef, Blake3, CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

/// What a mask is painted over: the Expand canvas feeding it, if any, and the image under that
/// (an Expand's margins don't change what was painted on).
pub struct Source<'a> {
    pub expand: Option<&'a Node>,
    pub image: Option<&'a AssetRef>,
}

fn input<'a>(canvas: &'a Canvas, node: NodeId, port: &str) -> Option<&'a Node> {
    let edge = canvas.edges().values().find(|e| e.to.node == node && e.to.port == port)?;
    canvas.nodes().get(&edge.from.node)
}

/// The image `mask` is painted over.
pub fn source(canvas: &Canvas, mask: NodeId) -> Source<'_> {
    match input(canvas, mask, "image") {
        Some(n) if n.kind == EXPAND => Source { expand: Some(n), image: input(canvas, n.id, "image").and_then(crate::split::image_of) },
        Some(n) => Source { expand: None, image: crate::split::image_of(n) },
        None => Source { expand: None, image: None },
    }
}

/// `asset` as a mask records it, once its size is known.
pub fn painted(asset: &AssetRef) -> Option<Painted> {
    let (width, height) = (asset.meta.width?, asset.meta.height?);
    (width > 0 && height > 0).then(|| Painted { hash: asset.hash.to_string(), width, height })
}

/// How a mask's strokes relate to the image under it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// Nothing to compare: no strokes, no recorded image (a mask saved before #546), or no image
    /// under it yet.
    Unbound,
    /// Painted over the image that is there.
    Current,
    /// The image changed (a new run or seed) but kept its shape: the strokes may no longer
    /// cover what they were painted on.
    Changed,
    /// The image changed shape: the strokes would stretch onto it.
    Reshaped,
}

impl Binding {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unbound => "unbound",
            Self::Current => "current",
            Self::Changed => "changed",
            Self::Reshaped => "reshaped",
        }
    }
}

/// How `mask`'s strokes relate to the image under it now. A mask that can't be read is
/// unbound: running it reports that error.
pub fn binding(canvas: &Canvas, mask: &Node) -> Binding {
    let Ok(stored) = strokes(mask) else { return Binding::Unbound };
    let (Some(over), false) = (stored.over, stored.strokes.is_empty()) else { return Binding::Unbound };
    let Some(image) = source(canvas, mask.id).image else { return Binding::Unbound };
    if image.hash.to_string() == over.hash {
        return Binding::Current;
    }
    match (image.meta.width, image.meta.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 && over.reshaped(w, h) => Binding::Reshaped,
        _ => Binding::Changed,
    }
}

/// `strokes` painted over a `from` image, fitted onto a `to` image of another shape: the old
/// image is scaled to fit inside the new one and centred, the brush scaling with it. The Expand
/// margins `m` keep their size in pixels on both.
pub fn fit(strokes: &[Stroke], from: (u32, u32), to: (u32, u32), m: Margins) -> Vec<Stroke> {
    let (w0, h0) = (f64::from(from.0.max(1)), f64::from(from.1.max(1)));
    let (w1, h1) = (f64::from(to.0.max(1)), f64::from(to.1.max(1)));
    let (left, top) = (f64::from(m.left), f64::from(m.top));
    let frame = |w: f64, h: f64| (w + left + f64::from(m.right), h + top + f64::from(m.bottom));
    let (old, new) = (frame(w0, h0), frame(w1, h1));
    let scale = (w1 / w0).min(h1 / h0);
    let offset = ((w1 - scale * w0) / 2.0, (h1 - scale * h0) / 2.0);
    let axis = |v: f64, old: f64, before: f64, offset: f64, new: f64| (((v * old - before) * scale + offset + before) / new).clamp(-1.0, 2.0);
    let brush = old.0.max(old.1) * scale / new.0.max(new.1);
    strokes
        .iter()
        .map(|s| Stroke {
            size: (s.size * brush).clamp(MIN_SIZE, 1.0),
            hardness: s.hardness,
            erase: s.erase,
            points: s.points.iter().map(|[x, y]| [axis(*x, old.0, left, offset.0, new.0), axis(*y, old.1, top, offset.1, new.1)]).collect(),
        })
        .collect()
}

/// Images whose transparency has been looked at, newest last.
static TRANSPARENT: Mutex<Vec<(Blake3, bool)>> = Mutex::new(Vec::new());
const REMEMBERED: usize = 32;

/// Whether `asset` has any transparent pixel. An image proven opaque on import isn't read; one
/// that can't be read counts as opaque, so nothing is marked on a guess.
pub(super) fn transparent(c: &Context<'_>, asset: &AssetRef) -> bool {
    if asset.meta.has_alpha == Some(false) {
        return false;
    }
    if let Some((_, seen)) = TRANSPARENT.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(hash, _)| *hash == asset.hash) {
        return *seen;
    }
    let native = match c.assets.get(&asset.hash) {
        Some(_) => None,
        None => c.assets.preview_read(asset).ok().and_then(|input| input.read_verified(&Default::default(), &Default::default()).ok()),
    };
    // Bytes not here yet may arrive later, so that isn't remembered; an image that won't decode
    // is remembered as opaque, so a stroke doesn't retry it.
    let Some(bytes) = c.assets.get(&asset.hash).or(native.as_deref()) else { return false };
    let seen = nodecraft_media::decode_image(bytes, &crate::action_exec::decode_limits())
        .map(|(_, image)| image.color().has_alpha() && image.into_rgba8().pixels().any(|p| p.0[3] < 255))
        .unwrap_or(false);
    let mut remembered = TRANSPARENT.lock().unwrap_or_else(PoisonError::into_inner);
    if remembered.len() >= REMEMBERED {
        remembered.remove(0);
    }
    remembered.push((asset.hash, seen));
    seen
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefitParams {
    canvas: CanvasId,
    id: NodeId,
    /// Fit the strokes onto the new image's shape; keep them as they are when false.
    fit: bool,
}

fn refit(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RefitParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| not_found("canvas"))?;
    let mask = canvas.nodes().get(&p.id).ok_or_else(|| not_found("document entity"))?;
    if mask.kind != ID {
        return Err(invalid("only a mask node is painted over an image"));
    }
    let stored = strokes(mask)?;
    if stored.strokes.is_empty() {
        return Err(invalid("nothing is painted on this mask"));
    }
    let under = source(canvas, p.id);
    let image = under.image.ok_or_else(|| invalid("run the image under the mask first"))?;
    let now = painted(image).ok_or_else(|| invalid("the image's size isn't known: run it again"))?;
    let moved = match (p.fit, &stored.over) {
        (false, _) => stored.strokes,
        (true, Some(old)) if !old.reshaped(now.width, now.height) => stored.strokes,
        (true, Some(old)) => fit(&stored.strokes, (old.width, old.height), (now.width, now.height), under.expand.map(margins).unwrap_or_default()),
        (true, None) => return Err(invalid("this mask doesn't record the image it was painted on, so it can't be fitted; keep the strokes instead")),
    };
    let json = Strokes { strokes: moved, over: Some(now) }.encode().map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the mask isn't valid: {e}")))?;
    let mut changed = mask.clone();
    changed.config.insert(CONFIG.into(), nodecraft_types::Value::Text(json));
    if changed == *mask {
        return Ok(Effect::None { result: json!({"changed": false}) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas: p.canvas, node: changed }]), result: json!({"changed": true}), gesture: None })
}

fn enabled_refit(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    refit(c, value).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusParams {
    canvas: CanvasId,
}

/// Most masks one status lists.
const MAX_LISTED: usize = 4096;

fn status(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: StatusParams = params(value)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| not_found("canvas"))?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Json::Null });
    }
    let masks: Vec<Json> = canvas
        .nodes()
        .values()
        .filter(|n| n.kind == ID)
        .filter_map(|n| match binding(canvas, n) {
            b @ (Binding::Changed | Binding::Reshaped) => Some(json!({"id": n.id, "binding": b.name()})),
            Binding::Unbound | Binding::Current => None,
        })
        .take(MAX_LISTED)
        .collect();
    Ok(Effect::None { result: json!({"masks": masks}) })
}

fn enabled_status(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    let p: StatusParams = params(value)?;
    c.document.canvases().get(&p.canvas).ok_or_else(|| not_found("canvas"))?;
    Ok(())
}

/// The masks a run of `nodes` on `canvas` would use while their image has changed shape: each
/// mask among them, and each feeding one of them directly. Masks in `inlined` (inside a
/// technique's version, keyed by inlined id) are skipped: the canvas can't refit them, and the
/// technique's author saw them over the image it was published with.
pub fn reshaped(canvas: &Canvas, nodes: impl IntoIterator<Item = NodeId>, inlined: &BTreeMap<NodeId, NodeId>) -> Vec<NodeId> {
    let mut seen = BTreeSet::new();
    let mut found = BTreeSet::new();
    for node in nodes {
        let fed = canvas.edges().values().filter(|e| e.to.node == node).map(|e| e.from.node);
        for id in std::iter::once(node).chain(fed) {
            if inlined.contains_key(&id) || !seen.insert(id) {
                continue;
            }
            if let Some(mask) = canvas.nodes().get(&id).filter(|n| n.kind == ID)
                && binding(canvas, mask) == Binding::Reshaped
            {
                found.insert(id);
            }
        }
    }
    found.into_iter().collect()
}

/// Why a run is blocked by a reshaped mask; fixed form, no titles.
pub fn reason(owner: NodeId) -> String {
    format!(
        "node {owner} [mask_reshaped]: the image this mask was painted over changed shape, so the strokes would stretch; keep them or fit them onto the new image first (mask.refit)"
    )
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "mask.refit",
            label: "Re-fit mask",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || {
                json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas", "id", "fit"],
                    "properties": {"canvas": uuid(), "id": uuid(), "fit": {"type": "boolean"}}})
            },
            params_doc: "{canvas, id: mask NodeId, fit: bool} -> {changed}; binds a painted mask to the image under it now, as one undo step. fit: false keeps the strokes where they are; fit: true fits them onto the new image's shape (the image they were painted over is scaled to fit inside the new one and centred). Needs the image's size: run it first.",
            enabled: enabled_refit,
            run: refit,
        },
        CommandSpec {
            id: "mask.status",
            label: "List masks over a changed image",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || {
                json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["canvas"],
                    "properties": {"canvas": uuid()}})
            },
            params_doc: "{canvas} -> {masks: [{id, binding: \"changed\"|\"reshaped\"}]}; the painted masks whose image changed since they were painted. A reshaped one blocks runs that use it until mask.refit. Masks saved before images were recorded are not listed. Read-only.",
            enabled: enabled_status,
            run: status,
        },
    ]
}
