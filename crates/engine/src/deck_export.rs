//! `deck.export` (#692): draws a Deck card's slides at its frame size, as the presenter lays them
//! out, and writes one slide as a PNG or the slides as one PDF. The command looks up what each
//! source layer shows; drawing waits for the export itself, which is native only. A source whose
//! node shows nothing, or whose image is missing, damaged or too large, draws as an empty box and
//! is reported, so an export never fails over one.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::deck_cmds;
use nodecraft_actions::deck::{self, Drawable};
use nodecraft_actions::layers::Frame;
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::PathBuf;

/// Most pixel bytes one PDF export draws, across its slides (each held until the file is
/// written).
pub const MAX_PDF_PIXEL_BYTES: u64 = 1 << 30;
/// Largest file an export writes.
pub const MAX_EXPORT_BYTES: usize = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlideFormat {
    Png,
    Pdf,
}

/// The slides an export draws, each with what its sources show as the export was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct SlidesExport {
    pub frame: Frame,
    pub format: SlideFormat,
    /// Each slide's id and what it draws.
    pub slides: Vec<(String, Drawable<AssetRef>)>,
    /// Each source whose node shows nothing, as (slide id, layer id).
    pub missing: Vec<(String, String)>,
    /// The card's bounded collection of imported fonts.
    pub fonts: Vec<(String, AssetRef)>,
}

impl SlidesExport {
    /// The export's result once `bytes` are written; `unreadable` are the image sources drawing
    /// found it couldn't use.
    pub(crate) fn result(&self, bytes: u64, unreadable: &[(String, String)], font_fallbacks: &[(String, String, String)]) -> Json {
        let missing: Vec<Json> = self.missing.iter().chain(unreadable).map(|(slide, layer)| json!({"slide": slide, "layer": layer})).collect();
        let mut result = json!({"bytes": bytes, "slides": self.slides.len(), "missing": missing});
        if !font_fallbacks.is_empty()
            && let Some(result) = result.as_object_mut()
        {
            result.insert(
                "font_fallbacks".into(),
                json!(
                    font_fallbacks
                        .iter()
                        .map(|(slide, layer, font)| json!({"slide": slide, "layer": layer, "font": font, "resolved": "lato"}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        result
    }
}

/// An export's bytes, and the image sources drawn as empty boxes because their image couldn't
/// be used.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Rendered {
    pub bytes: Vec<u8>,
    pub unreadable: Vec<(String, String)>,
    pub font_fallbacks: Vec<(String, String, String)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportParams {
    canvas: CanvasId,
    id: NodeId,
    format: SlideFormat,
    /// The one slide to export; a PNG needs it, a PDF without it has every slide.
    #[serde(default)]
    slide: Option<String>,
    path: PathBuf,
    #[serde(default)]
    overwrite: bool,
}

fn export(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExportParams = params(value)?;
    crate::export::check(&p.path)?;
    let node = deck_cmds::card(c, p.canvas, p.id)?;
    let on = deck_cmds::canvas(c, p.canvas)?;
    let stored = deck_cmds::stored(node)?;
    let frame = deck_cmds::frame(c, node)?;
    let chosen: Vec<&deck::Slide> = match (&p.slide, p.format) {
        (Some(id), _) => vec![
            stored
                .slides
                .get(stored.index_of(id).ok_or_else(|| Error::new(ErrorKind::NotFound, "slide does not exist"))?)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "slide does not exist"))?,
        ],
        (None, SlideFormat::Png) => return Err(invalid("a PNG holds one slide; name it with slide")),
        (None, SlideFormat::Pdf) => stored.slides.iter().collect(),
    };
    if chosen.is_empty() {
        return Err(invalid("the deck has no slides to export"));
    }
    let pixels = u64::from(frame.width).saturating_mul(u64::from(frame.height)).saturating_mul(4);
    if pixels.saturating_mul(chosen.len() as u64) > MAX_PDF_PIXEL_BYTES {
        return Err(invalid("the deck is too large to export as one PDF; export fewer slides, or make the deck smaller"));
    }
    let mut slides = Vec::with_capacity(chosen.len());
    let mut missing = Vec::new();
    for slide in chosen {
        let drawn = deck::drawable(slide, frame, |origin, nth| deck_cmds::shown(on, origin, nth));
        missing.extend(drawn.missing.iter().map(|layer| (slide.id.clone(), layer.clone())));
        slides.push((slide.id.clone(), drawn));
    }
    let what = SlidesExport { frame, format: p.format, slides, missing, fonts: deck_cmds::fonts::held(node) };
    Ok(Effect::Export { what: crate::export::Exported::Slides(Box::new(what)), path: p.path, overwrite: p.overwrite })
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    export(c, value).map(|_| ())
}

/// The file's bytes: the one slide as a PNG, or every slide as a page of a PDF.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn render(assets: &nodecraft_format::AssetStore, what: &SlidesExport) -> Result<Rendered, Error> {
    use nodecraft_actions::image::ImageLimits;
    use nodecraft_actions::layers::{Content, Fonts, font_info};
    let limits = ImageLimits::default();
    // Use the editor's validation and bounds before retaining font payloads for the export.
    let payloads: Vec<_> = what
        .fonts
        .iter()
        .take(nodecraft_actions::layers::MAX_FONTS)
        .filter_map(|(name, asset)| {
            let bytes = crate::layer_cmds::fonts::bytes(assets, asset).ok()?;
            font_info(&bytes).ok()?;
            Some((name, bytes))
        })
        .collect();
    let mut fonts = Fonts::default();
    for (name, bytes) in &payloads {
        let _ = fonts.add(name, bytes);
    }
    let too_large = || Error::new(ErrorKind::InvalidInput, "the export is too large to write; export fewer slides, or make the deck smaller");
    let mut pages = Vec::new();
    let mut unreadable = Vec::new();
    let mut font_fallbacks = Vec::new();
    for (slide, drawn) in &what.slides {
        for layer in &drawn.layers {
            if layer.visible
                && layer.opacity > 0.0
                && let Content::Text { font, .. } = &layer.content
                && !fonts.has(font)
            {
                font_fallbacks.push((slide.clone(), layer.id.clone(), font.clone()));
            }
        }
        let (raster, boxed) = draw(assets, what.frame, drawn, &fonts, limits)?;
        unreadable.extend(boxed.into_iter().map(|layer| (slide.clone(), layer)));
        pages.push(nodecraft_media::PageImage { width: raster.width(), height: raster.height(), rgba: raster.pixels().to_vec() });
    }
    let bytes = match (what.format, pages.as_slice()) {
        (SlideFormat::Png, [page]) => nodecraft_media::encode_png(page.width, page.height, &page.rgba, &crate::action_exec::decode_limits())
            .map_err(|e| Error::new(ErrorKind::Internal, format!("the slide can't be encoded: {e}")))?,
        (SlideFormat::Png, _) => return Err(invalid("a PNG holds one slide")),
        (SlideFormat::Pdf, _) => nodecraft_media::images_to_pdf(pages).map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?,
    };
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(too_large());
    }
    Ok(Rendered { bytes, unreadable, font_fallbacks })
}

/// One slide drawn at its frame size, and the image layers it drew as empty boxes because their
/// image couldn't be used. A source with nothing to show is an empty grey box too.
#[cfg(not(target_arch = "wasm32"))]
fn draw(
    assets: &nodecraft_format::AssetStore,
    frame: Frame,
    drawn: &Drawable<AssetRef>,
    fonts: &nodecraft_actions::layers::Fonts<'_>,
    limits: nodecraft_actions::image::ImageLimits,
) -> Result<(nodecraft_actions::image::Raster, Vec<String>), Error> {
    use nodecraft_actions::layers::{self, Content, Layer, Layers, ShapeKind};
    let mut images = Vec::with_capacity(drawn.images.len());
    let mut decoded = 0u64;
    for asset in &drawn.images {
        // An image that can't be used is an empty box, as the presenter shows it.
        let Ok(image) = raster(assets, asset, limits) else {
            images.push(None);
            continue;
        };
        decoded = decoded.saturating_add(image.pixels().len() as u64);
        if decoded > limits.working_bytes {
            return Err(Error::new(ErrorKind::InvalidInput, "a slide's images are too large to draw together; make some smaller first (Resize)"));
        }
        images.push(Some(image));
    }
    let mut unreadable = Vec::new();
    let empty = |layer: &Layer| Layer {
        content: Content::Shape { shape: ShapeKind::Rectangle, fill: None, stroke: Some(EMPTY_BOX.into()), stroke_width: 2.0 },
        ..layer.clone()
    };
    let scene: Vec<Layer> = drawn
        .layers
        .iter()
        .map(|layer| match layer.content {
            Content::Source { .. } => empty(layer),
            Content::Image { input, .. } if images.get(input as usize).is_none_or(Option::is_none) => {
                unreadable.push(layer.id.clone());
                empty(layer)
            }
            _ => layer.clone(),
        })
        .collect();
    let raster = layers::composite(frame, &Layers { layers: scene }, &images, fonts, 1.0, limits)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the slide can't be drawn: {e}")))?;
    Ok((raster, unreadable))
}

/// The outline a source with nothing to show draws as: a plain grey outline on the slide, where
/// the editor's stage draws its own placeholder.
#[cfg(not(target_arch = "wasm32"))]
const EMPTY_BOX: &str = "#9a9a9a";

/// An image asset decoded through the same verified, bounded path as the image actions.
#[cfg(not(target_arch = "wasm32"))]
fn raster(
    assets: &nodecraft_format::AssetStore,
    asset: &AssetRef,
    limits: nodecraft_actions::image::ImageLimits,
) -> Result<nodecraft_actions::image::Raster, Error> {
    let unreadable = || Error::new(ErrorKind::InvalidInput, "an image on a slide can't be read");
    assets.validate_reference(asset)?;
    let native = if assets.get(&asset.hash).is_none() {
        let read = assets.preview_read(asset).map_err(|error| match error {
            nodecraft_format::PreviewReadError::Missing => Error::new(ErrorKind::NotFound, "an image on a slide isn't in this project"),
            _ => unreadable(),
        })?;
        Some(read.read_verified(&Default::default(), &Default::default()).map_err(|_| unreadable())?)
    } else {
        None
    };
    let bytes = native.as_deref().or_else(|| assets.get(&asset.hash)).ok_or_else(unreadable)?;
    let decode = crate::action_exec::decode_limits();
    let meta = nodecraft_media::probe_image(bytes, &decode).map_err(|_| unreadable())?;
    if u64::from(meta.width).saturating_mul(u64::from(meta.height)) > limits.pixels {
        return Err(Error::new(ErrorKind::InvalidInput, format!("an image on a slide is larger than {} megapixels", limits.pixels / 1_000_000)));
    }
    let (_, decoded) = nodecraft_media::decode_image(bytes, &decode).map_err(|_| unreadable())?;
    let rgba = decoded.into_rgba8();
    nodecraft_actions::image::Raster::new(rgba.width(), rgba.height(), rgba.into_raw(), limits).map_err(|_| unreadable())
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "format", "path"],
        "properties": {"canvas": uuid, "id": uuid, "format": {"enum": ["png", "pdf"]},
            "slide": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$"},
            "path": {"type": "string", "minLength": 1, "maxLength": crate::export::MAX_PATH_BYTES},
            "overwrite": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "deck.export",
        label: "Export slides…",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id: Deck NodeId, format: \"png\" | \"pdf\", slide?: slide id (required for png), path: absolute file path, overwrite?: bool (default false)} -> {bytes, slides, missing: [{slide, layer}], font_fallbacks?: [{slide, layer, font, resolved: \"lato\"}]}; draws slides at the deck's size, as the presenter shows them, and writes one slide as a PNG, or the named slide or every slide as the pages of one PDF. A source layer whose node shows nothing, or whose image is missing, damaged or too large, draws as an empty box and is listed in missing. Imported deck fonts draw as in the presenter; missing or unreadable fonts use Lato and each visible affected text layer is listed in font_fallbacks. Refuses a file that's already there unless `overwrite`, a missing folder, a full disk, and a PDF over 1 GiB of pixels, leaving nothing half written. Desktop only.",
        enabled,
        run: export,
    }]
}

#[cfg(test)]
mod tests;
