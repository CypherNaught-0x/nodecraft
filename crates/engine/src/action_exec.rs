//! Running action jobs (#410): the host's [`ActionExecutor`] for the run executor. Each action
//! kind's visible parameters map onto the request of the matching operation in
//! `nodecraft-actions`; inputs come from the project's assets and text values. New images are
//! encoded as PNG and staged here; a later action in the same run reads them from here (chains),
//! and the host imports the staged assets ([`LocalActions::take_staged`]) before recording the
//! run, so a result never names an asset the project doesn't hold. A failed or cancelled run's
//! staged images are dropped with it. Nothing here touches the document.
//!
//! Image work is CPU work: the host runs action jobs off the UI thread (a worker runtime), and
//! the GPU path (#89) is a later step.
use nodecraft_actions::image::{CpuExecutor, ImageLimits, ImageRequest, ImageResult, Raster};
use nodecraft_actions::text::{self, ConcatOptions, MatchMode, ReplaceOptions, SplitMode, TextRequest, TextResult};
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_types::{AssetRef, MediaMeta, ParamValues, Value};
use serde_json::{Map, Value as Json, json};
use std::sync::{Mutex, PoisonError};

/// Actions run against a snapshot of the project's assets.
pub struct LocalActions {
    assets: AssetStore,
    staged: Mutex<AssetStore>,
}

impl LocalActions {
    /// Actions reading `assets` (a cheap clone of the session's store: buffers are shared).
    pub fn new(assets: AssetStore) -> Self {
        Self { assets, staged: Mutex::new(AssetStore::default()) }
    }

    /// The images actions made since the last call, for the host to import.
    pub fn take_staged(&self) -> AssetStore {
        std::mem::take(&mut *self.staged.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

fn number(params: &ParamValues, name: &str) -> Result<f64, String> {
    match params.get(name) {
        Some(Value::Number(n)) => Ok(n.get()),
        _ => Err(format!("the {name} setting is missing")),
    }
}

fn whole(params: &ParamValues, name: &str) -> Result<u64, String> {
    match params.get(name) {
        Some(Value::Unsigned(n)) => Ok(n.0),
        _ => Err(format!("the {name} setting is missing")),
    }
}

fn word<'a>(params: &'a ParamValues, name: &str) -> Result<&'a str, String> {
    match params.get(name) {
        Some(Value::Text(t)) => Ok(t),
        _ => Err(format!("the {name} setting is missing")),
    }
}

fn flag(params: &ParamValues, name: &str) -> Result<bool, String> {
    match params.get(name) {
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(format!("the {name} setting is missing")),
    }
}

/// `#rrggbb` or `#rrggbbaa` as the actions' colour.
fn color(text: &str) -> Result<Json, String> {
    let hex =
        text.strip_prefix('#').filter(|h| matches!(h.len(), 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("a colour is #rrggbb or #rrggbbaa")?;
    let byte = |i: usize| hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()).ok_or("a colour is #rrggbb or #rrggbbaa");
    let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
    Ok(json!({"r": byte(0)?, "g": byte(2)?, "b": byte(4)?, "a": alpha}))
}

/// The image request for action `action` with `params`.
fn image_request(action: &str, p: &ParamValues) -> Result<ImageRequest, String> {
    let body = match action {
        "image.blur" => json!({"options": {"kind": "gaussian", "sigma": number(p, "sigma")?}}),
        "image.grade" => {
            json!({"options": {"warmth": number(p, "warmth")?, "contrast": number(p, "contrast")?, "saturation": number(p, "saturation")?, "brightness": number(p, "brightness")?}})
        }
        "image.filter" => json!({"options": {"preset": {"kind": word(p, "preset")?}, "intensity": number(p, "intensity")?}}),
        "image.tint" => json!({"options": {"color": color(word(p, "color")?)?, "intensity": number(p, "intensity")?, "blend": word(p, "blend")?}}),
        "image.flip" => json!({"axis": word(p, "axis")?}),
        "image.rotate" => {
            json!({"options": {"angle": number(p, "angle")?, "expand": flag(p, "expand")?, "background": {"r": 0, "g": 0, "b": 0, "a": 0}, "resampling": word(p, "resampling")?}})
        }
        "image.crop" => {
            // The crop stays inside the image: moving its corner shrinks what's left of it.
            let (x, y) = (number(p, "x")?.clamp(0.0, 0.99), number(p, "y")?.clamp(0.0, 0.99));
            json!({"options": {"kind": "normalized", "x": x, "y": y, "width": number(p, "width")?.min(1.0 - x), "height": number(p, "height")?.min(1.0 - y)}})
        }
        "image.resize" => json!({"options": {"size": {"kind": "scale", "factor": number(p, "factor")?}, "resampling": word(p, "resampling")?}}),
        _ => return Err(format!("{action} isn't an image action this host runs")),
    };
    let mut body: Map<String, Json> = match body {
        Json::Object(map) => map,
        _ => Map::new(),
    };
    body.insert("action".into(), Json::String(action.into()));
    serde_json::from_value(Json::Object(body)).map_err(|_| "the action's settings are out of range".to_owned())
}

/// Every text value in `inputs`, one list level flattened (what a list port carries), in order.
fn texts(inputs: &[Value]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for value in inputs {
        match value {
            Value::Text(t) => out.push(t.clone()),
            Value::List(items) => {
                for item in items {
                    match item {
                        Value::Text(t) => out.push(t.clone()),
                        _ => return Err("a text action takes text".into()),
                    }
                }
            }
            _ => return Err("a text action takes text".into()),
        }
    }
    Ok(out)
}

fn text_request(action: &str, p: &ParamValues, inputs: &[Value]) -> Result<TextRequest, String> {
    let mut all = texts(inputs)?;
    let first = || all.first().cloned().ok_or_else(|| "connect a text input first".to_owned());
    Ok(match action {
        "text.split" => {
            let mode = match word(p, "mode")? {
                "separator" => match word(p, "separator")? {
                    "" => return Err("enter a separator to split by".into()),
                    separator => SplitMode::Separator(separator.to_owned()),
                },
                "regex" => SplitMode::Regex(word(p, "pattern")?.to_owned()),
                "lines" => SplitMode::Lines(usize::try_from(whole(p, "lines")?).map_err(|_| "too many lines per part")?),
                "chars" => SplitMode::Chars(usize::try_from(whole(p, "chars")?).map_err(|_| "too many characters per part")?),
                "paragraphs" => SplitMode::Paragraphs,
                _ => return Err("unknown split mode".into()),
            };
            let max_parts = usize::try_from(whole(p, "max_parts")?).map_err(|_| "too many parts")?;
            TextRequest::Split { input: first()?, mode, max_parts }
        }
        "text.replace" => {
            let find = word(p, "find")?.to_owned();
            let pattern = if flag(p, "regex")? { MatchMode::Regex(find) } else { MatchMode::Literal(find) };
            let options = ReplaceOptions {
                pattern,
                replacement: word(p, "replacement")?.to_owned(),
                case_sensitive: flag(p, "case_sensitive")?,
                whole_word: flag(p, "whole_word")?,
            };
            TextRequest::Replace { input: first()?, options }
        }
        "text.concat" => {
            let options = ConcatOptions {
                separator: word(p, "separator")?.to_owned(),
                prefix: word(p, "prefix")?.to_owned(),
                suffix: word(p, "suffix")?.to_owned(),
                trim: flag(p, "trim")?,
                skip_empty: flag(p, "skip_empty")?,
            };
            TextRequest::Concat { inputs: std::mem::take(&mut all), options }
        }
        _ => return Err(format!("{action} isn't a text action this host runs")),
    })
}

impl LocalActions {
    fn image(&self, action: &str, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        let request = image_request(action, params)?;
        let Some(Value::Media(source)) = inputs.first() else { return Err("connect an image first".into()) };
        // The project's images, or one an earlier action in this run made (chains).
        let staged = self.staged.lock().unwrap_or_else(PoisonError::into_inner).get(&source.hash).map(<[u8]>::to_vec);
        let bytes = match &staged {
            Some(bytes) => bytes.as_slice(),
            None => self.assets.get(&source.hash).ok_or("the input image isn't in this project")?,
        };
        let actions = ImageLimits::default();
        // Bounded before decoding: the pixel count the actions accept, at four bytes each, plus
        // the decoder's own working copy.
        let rgba_bytes = actions.pixels.saturating_mul(4);
        let limits = nodecraft_media::Limits {
            max_width: actions.edge,
            max_height: actions.edge,
            max_alloc: rgba_bytes.saturating_mul(2),
            ..nodecraft_media::Limits::default()
        };
        let meta = nodecraft_media::probe_image(bytes, &limits).map_err(|e| format!("the input image can't be read: {e}"))?;
        if u64::from(meta.width).saturating_mul(u64::from(meta.height)) > actions.pixels {
            return Err(format!("the input image is larger than {} megapixels", actions.pixels / 1_000_000));
        }
        let (_, decoded) = nodecraft_media::decode_image(bytes, &limits).map_err(|e| format!("the input image can't be read: {e}"))?;
        let rgba = decoded.into_rgba8();
        let raster = Raster::new(rgba.width(), rgba.height(), rgba.into_raw(), actions).map_err(|e| format!("the image can't be processed: {e}"))?;
        let result = request
            .execute(nodecraft_actions::image::ImageInputs::single(&raster), &mut CpuExecutor::serial(), actions)
            .map_err(|e| format!("{action} failed: {e}"))?;
        let ImageResult::Image(out) = result else { return Err(format!("{action} made more than one image")) };
        let png = nodecraft_media::encode_png(out.width(), out.height(), out.pixels(), &limits).map_err(|e| format!("the result can't be encoded: {e}"))?;
        if png.len() > nodecraft_format::MAX_ASSET_BYTES {
            return Err(format!("the result is larger than {} MiB; make it smaller first (Resize)", nodecraft_format::MAX_ASSET_BYTES / (1024 * 1024)));
        }
        let hash = self.staged.lock().unwrap_or_else(PoisonError::into_inner).insert(&png).map_err(|e| e.message)?;
        let mime = "image/png".parse().map_err(|_| "image/png isn't a media type".to_owned())?;
        Ok(Value::Media(AssetRef { hash, mime, meta: MediaMeta { width: Some(out.width()), height: Some(out.height()), ..MediaMeta::default() } }))
    }
}

impl ActionExecutor for LocalActions {
    fn execute(&self, action: &str, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        if action.starts_with("image.") {
            return self.image(action, params, inputs);
        }
        // A Replace node with nothing to find yet passes its text through.
        if action == "text.replace" && params.get("find").is_some_and(|f| *f == Value::Text(String::new())) {
            return texts(inputs)?.into_iter().next().map(Value::Text).ok_or_else(|| "connect a text input first".to_owned());
        }
        let request = text_request(action, params, inputs)?;
        match request.execute(text::Limits::default()).map_err(|e| format!("{action} failed: {e}"))? {
            TextResult::Text(t) => Ok(Value::Text(t)),
            TextResult::Parts(parts) => Ok(Value::List(parts.into_iter().map(Value::Text).collect())),
        }
    }
}

#[cfg(test)]
mod tests;
