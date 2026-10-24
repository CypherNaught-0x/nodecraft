//! The Layer Editor's executor (#101): decodes the connected images, matches each stored layer
//! to its image, and composites them into one PNG with the fonts the node holds (#548).
use super::{LocalActions, flag, whole, word};
use nodecraft_actions::image::ImageLimits;
use nodecraft_actions::layers::{self, Frame, Layers};
use nodecraft_types::{ParamValues, Value};

/// The frame a Layer Editor node's parameters describe.
pub(crate) fn frame(params: &ParamValues) -> Result<Frame, String> {
    let edge = |name| {
        whole(params, name).and_then(|v| {
            u32::try_from(v).ok().filter(|v| (layers::MIN_FRAME..=layers::MAX_FRAME).contains(v)).ok_or(format!("the {name} is 100 to 4096 pixels"))
        })
    };
    let background =
        if flag(params, "transparent")? { [0, 0, 0, 0] } else { layers::parse_color(word(params, "background")?).map_err(|_| "the background is #rrggbb")? };
    Ok(Frame { width: edge("width")?, height: edge("height")?, background })
}

/// Each input's origin node, as planned (empty where it isn't known).
pub(crate) fn sources(params: &ParamValues) -> Vec<String> {
    match params.get(nodecraft_nodes::layers::SOURCES_PARAM) {
        Some(Value::List(items)) => items.iter().map(|v| if let Value::Text(t) = v { t.clone() } else { String::new() }).collect(),
        _ => Vec::new(),
    }
}

impl LocalActions {
    /// The bytes of each font the node holds, by name; a font that can't be read is left out.
    fn fonts(&self, params: &ParamValues) -> Vec<(String, Vec<u8>)> {
        let Some(Value::List(fonts)) = params.get(nodecraft_nodes::layers::FONTS_PARAM) else { return Vec::new() };
        fonts
            .iter()
            .take(layers::MAX_FONTS)
            .filter_map(|font| {
                let Value::List(pair) = font else { return None };
                let (Some(Value::Text(name)), Some(media @ Value::Media(asset))) = (pair.first(), pair.get(1)) else { return None };
                // A font larger than any font may be is left out before any of it is read.
                if self.stored_len(asset)? > layers::MAX_FONT_BYTES as u64 {
                    return None;
                }
                let bytes = self.media_bytes(Some(media), "font").ok()?;
                (bytes.len() <= layers::MAX_FONT_BYTES).then(|| (name.clone(), bytes))
            })
            .collect()
    }

    /// How large an asset is, without reading it; None when it isn't in this project.
    fn stored_len(&self, asset: &nodecraft_types::AssetRef) -> Option<u64> {
        if let Some(resident) = self.assets.get(&asset.hash) {
            return u64::try_from(resident.len()).ok();
        }
        self.assets.preview_read(asset).ok().map(|read| read.byte_len())
    }

    pub(super) fn layers(&self, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        let json = match params.get(nodecraft_nodes::layers::SPEC_PARAM) {
            None => nodecraft_nodes::layers::EMPTY,
            Some(Value::Text(json)) => json,
            Some(_) => return Err("the layers can't be read".into()),
        };
        let stored = Layers::parse(json).map_err(|e| format!("the layers can't be read: {e}"))?;
        let frame = frame(params)?;
        let mut sources = sources(params);
        sources.resize(inputs.len(), String::new());
        let resolved = stored.resolve(&sources, frame).map_err(|e| format!("the layers can't be drawn: {e}"))?;
        // Decode only what a visible layer shows, and stop before the decoded images alone are
        // over the working budget the compositor would refuse anyway.
        let limits = ImageLimits::default();
        let mut decoded = 0u64;
        let mut images = Vec::new();
        for (value, shown) in inputs.iter().zip(resolved.shown(inputs.len())) {
            images.push(if shown {
                let image = self.raster(Some(value))?;
                decoded = decoded.saturating_add(image.pixels().len() as u64);
                if decoded > limits.working_bytes {
                    return Err("the connected images are too large to draw together; make some smaller first (Resize)".into());
                }
                Some(image)
            } else {
                None
            });
        }
        let bytes = self.fonts(params);
        let mut fonts = layers::Fonts::default();
        for (name, bytes) in &bytes {
            // A font that can't be read draws as Lato, as a missing one does.
            let _ = fonts.add(name, bytes);
        }
        let out = layers::composite(frame, &resolved, &images, &fonts, 1.0, limits).map_err(|e| format!("image.layers failed: {e}"))?;
        self.stage(&out)
    }
}
