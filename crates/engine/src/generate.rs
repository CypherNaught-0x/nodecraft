//! Bounded conversion between standalone model jobs and provider requests/results.
//!
//! Complete borrowed admission precedes owned conversion, including late semantic failures.
//! Limits are conservative representation/work budgets, not exact JSON bytes or allocator/RSS
//! guarantees. Provider-owned inputs/results remain with the caller when conversion is refused.

use nodecraft_format::{
    AssetStore,
    asset_source::{StreamMetrics, TransferCancel},
};
use nodecraft_graph::{MAX_NODE_BYTES, MAX_VALUE_DEPTH, MAX_VALUE_ITEMS};
use nodecraft_media::{ContentHash, MediaKind};
use nodecraft_nodes::{GenerateOutput, JobSpec};
use nodecraft_providers::{Completed, JobInput, Output, ProviderJob};
use nodecraft_types::{AssetRef, Blake3, Error, ErrorKind, Modality, Value};
use serde_json::{Map, Value as Json};
use std::sync::Arc;

/// Largest individual binary result and aggregate raw result payload accepted.
pub const MAX_INLINE_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
// Matches graph's per-value aggregate text limit; parity is pinned by a Generation test.
const MAX_TEXT_VALUE_BYTES: usize = 1024 * 1024;
/// Largest aggregate size of the media one model call sends (#502).
pub const MAX_MEDIA_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Where a model call's media inputs come from (#502). `size` answers from metadata without
/// reading content, so an oversized call is refused before any bytes are read; `read` returns
/// the stored, verified bytes and refuses an asset larger than `limit`.
pub trait MediaSource {
    fn size(&self, asset: &AssetRef) -> Result<u64, Error>;
    fn read(&self, asset: &AssetRef, limit: u64) -> Result<Arc<[u8]>, Error>;
}
impl MediaSource for AssetStore {
    fn size(&self, asset: &AssetRef) -> Result<u64, Error> {
        self.descriptor(&asset.hash).map(|d| d.bytes).ok_or_else(|| invalid("a media input names an asset this project doesn't have"))
    }
    fn read(&self, asset: &AssetRef, limit: u64) -> Result<Arc<[u8]>, Error> {
        if self.size(asset)? > limit {
            return Err(invalid("a media input is larger than this model call allows"));
        }
        let metrics = StreamMetrics::default();
        let lease = self.open_verified(asset, &TransferCancel::default(), &metrics)?;
        if lease.byte_len() > limit {
            return Err(invalid("a media input is larger than this model call allows"));
        }
        let length = usize::try_from(lease.byte_len()).map_err(|_| invalid("a media input is larger than this model call allows"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| Error::new(ErrorKind::Internal, "not enough memory to read a media input"))?;
        bytes.resize(length, 0);
        std::io::Read::read_exact(&mut lease.reader(metrics), &mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "a media input could not be read"))?;
        Ok(bytes.into())
    }
}

#[derive(Clone, Copy)]
struct Limits {
    depth: usize,
    items: usize,
    inputs: usize,
    text: usize,
    request: usize,
    inline: usize,
    raw_outputs: usize,
    outputs: usize,
    media: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            depth: MAX_VALUE_DEPTH,
            items: MAX_VALUE_ITEMS,
            inputs: MAX_VALUE_ITEMS,
            text: MAX_TEXT_VALUE_BYTES,
            request: nodecraft_exec::planner::MAX_STEP_INPUT_BYTES,
            inline: MAX_INLINE_OUTPUT_BYTES,
            raw_outputs: MAX_INLINE_OUTPUT_BYTES,
            outputs: MAX_NODE_BYTES,
            media: MAX_MEDIA_INPUT_BYTES,
        }
    }
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn unsupported(message: &str) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

/// Every string costs 6*UTF8 bytes +34 (escaping, quotes, String envelope). A request Value
/// costs 128 even when empty; each input root/leaf costs 64 plus its role string. Output routes
/// cost 64+name, records 128, and media references 512+sniffed MIME+port. Claimed MIME is charged
/// as work even when not retained. These deliberately overcharge short encodings and empty work.
struct Budget(usize);
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.0 = self.0.checked_sub(bytes).ok_or_else(|| invalid("model conversion exceeds its representation byte limit"))?;
        Ok(())
    }
    fn string(&mut self, text: &str) -> Result<(), Error> {
        let bytes = text.len().checked_mul(6).and_then(|n| n.checked_add(34)).ok_or_else(|| invalid("model conversion byte count overflowed"))?;
        self.charge(bytes)
    }
}

// Instance-local instrumentation at real construction sites. Empty on production builds;
// admission never receives this object, so tests can distinguish scanning from copying.
#[derive(Default, Debug, PartialEq, Eq)]
struct Conversion {
    #[cfg(test)]
    strings: usize,
    #[cfg(test)]
    text_bytes: usize,
    #[cfg(test)]
    json_nodes: usize,
    #[cfg(test)]
    input_records: usize,
    #[cfg(test)]
    output_records: usize,
    #[cfg(test)]
    asset_arcs: usize,
    #[cfg(test)]
    hash_bytes: usize,
}
impl Conversion {
    fn string(&mut self, source: &str) -> String {
        #[cfg(test)]
        {
            self.strings += 1;
            self.text_bytes += source.len();
        }
        source.to_owned()
    }
    fn scalar_text(&mut self, value: Json) -> String {
        let text = value.to_string();
        #[cfg(test)]
        {
            self.strings += 1;
            self.text_bytes += text.len();
        }
        text
    }
}

struct RequestBudget<'a> {
    bytes: Budget,
    values: usize,
    inputs: usize,
    limits: Limits,
    source: Option<&'a dyn MediaSource>,
    media: u64,
}
impl RequestBudget<'_> {
    fn value(&mut self, root: &Value, role: Option<&str>) -> Result<(), Error> {
        let mut stack = vec![std::slice::from_ref(root).iter()];
        let mut text_bytes = 0usize;
        while let Some(items) = stack.last_mut() {
            let Some(value) = items.next() else {
                stack.pop();
                continue;
            };
            if stack.len().saturating_sub(1) > self.limits.depth {
                return Err(invalid("model request exceeds value depth limit"));
            }
            self.values = self.values.checked_add(1).ok_or_else(|| invalid("model request item count overflowed"))?;
            if self.values > self.limits.items {
                return Err(invalid("model request exceeds aggregate value item limit"));
            }
            self.bytes.charge(128)?;
            match value {
                Value::List(children) => {
                    if children.len() > self.limits.items.saturating_sub(self.values) {
                        return Err(invalid("model request exceeds aggregate value item limit"));
                    }
                    if !children.is_empty() {
                        if stack.len() > self.limits.depth {
                            return Err(invalid("model request exceeds value depth limit"));
                        }
                        stack.push(children.iter());
                    }
                    continue;
                }
                Value::Text(text) => {
                    text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| invalid("model request text count overflowed"))?;
                    if text_bytes > self.limits.text {
                        return Err(invalid("model request value exceeds text limit"));
                    }
                    self.bytes.string(text)?;
                }
                Value::Number(number) if !f64::from(*number).is_finite() => return Err(invalid("a parameter is not a finite number")),
                Value::Media(asset) => match (role, self.source) {
                    (None, _) => return Err(unsupported("a media value can't be a model parameter")),
                    (Some(_), None) => return Err(unsupported("this model call has no asset store to send media inputs from")),
                    (Some(_), Some(source)) => {
                        let size = source.size(asset)?;
                        self.media = self.media.checked_add(size).ok_or_else(|| invalid("model media size overflowed"))?;
                        if self.media > self.limits.media {
                            return Err(invalid("the model call's media inputs are larger than it allows"));
                        }
                        self.bytes.charge(512)?;
                        self.bytes.string(asset.mime.essence())?;
                    }
                },
                _ => {}
            }
            if let Some(role) = role {
                self.inputs = self.inputs.checked_add(1).ok_or_else(|| invalid("model input count overflowed"))?;
                if self.inputs > self.limits.inputs {
                    return Err(invalid("the model call has too many inputs"));
                }
                self.bytes.charge(64)?;
                self.bytes.string(role)?;
            }
        }
        Ok(())
    }
}
struct AdmittedRequest<'a> {
    spec: &'a JobSpec,
    key: &'a str,
}
/// Counts `spec`'s request against `limits`: the budget as far as it got, and the first
/// refusal. Advisory checks and runtime admission share this, so they count alike.
fn measure<'a>(spec: &JobSpec, key: &str, limits: Limits, source: Option<&'a dyn MediaSource>) -> (RequestBudget<'a>, Result<(), Error>) {
    let mut budget = RequestBudget { bytes: Budget(limits.request), values: 0, inputs: 0, limits, source, media: 0 };
    let result = (|| {
        let JobSpec::Generate { provider, model, params, inputs, .. } = spec else { return Err(invalid("not a model call")) };
        if params.len() > limits.items || inputs.len() > limits.items {
            return Err(invalid("model request exceeds item limit"));
        }
        for text in [provider.as_str(), model.as_str(), key] {
            budget.bytes.string(text)?;
        }
        for (name, value) in params {
            budget.bytes.string(name)?;
            budget.value(value, None)?;
        }
        for input in inputs {
            budget.bytes.charge(64)?;
            budget.bytes.string(&input.role)?;
            budget.value(&input.value, Some(&input.role))?;
        }
        Ok(())
    })();
    (budget, result)
}
fn admit_request<'a>(spec: &'a JobSpec, key: &'a str, limits: Limits, source: Option<&dyn MediaSource>) -> Result<AdmittedRequest<'a>, Error> {
    measure(spec, key, limits, source).1?;
    Ok(AdmittedRequest { spec, key })
}

/// The longest idempotency key a model call is sent with.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 64;

/// What a model request counts against the limits [`provider_job`] admits it under (#307):
/// aggregate values (parameters and inputs, nested lists and their items), flattened inputs,
/// the representation budget (strings, metadata and repeated roles) and stored media bytes.
/// When `refusal` is set the counts stop where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestCheck {
    pub values: usize,
    pub value_limit: usize,
    pub inputs: usize,
    pub input_limit: usize,
    pub bytes: usize,
    pub byte_limit: usize,
    pub media_bytes: u64,
    pub media_limit: u64,
    /// Why runtime admission would refuse the request; `None` when it would admit it.
    pub refusal: Option<Error>,
}

/// Checks `spec` as [`provider_job_with_media`] would admit it, without converting or copying
/// it, reading media content or calling a provider. A request it admits with `key` is admitted
/// at runtime with that key or any shorter one; [`check_request`] checks with the longest.
/// Advisory only: runtime admission still runs before conversion.
pub fn check_request_with_key(spec: &JobSpec, key: &str, source: Option<&dyn MediaSource>) -> RequestCheck {
    let limits = Limits::default();
    let (budget, result) = measure(spec, key, limits, source);
    RequestCheck {
        values: budget.values,
        value_limit: limits.items,
        inputs: budget.inputs,
        input_limit: limits.inputs,
        bytes: limits.request.saturating_sub(budget.bytes.0),
        byte_limit: limits.request,
        media_bytes: budget.media,
        media_limit: limits.media,
        refusal: result.err(),
    }
}

/// [`check_request_with_key`] with the longest idempotency key, so a request it admits is
/// admitted whatever key the run sends it with.
pub fn check_request(spec: &JobSpec, source: Option<&dyn MediaSource>) -> RequestCheck {
    const LONGEST: &str = "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk";
    const _: () = assert!(LONGEST.len() == MAX_IDEMPOTENCY_KEY_BYTES);
    check_request_with_key(spec, LONGEST, source)
}

// Recursive conversion is only reached after complete standalone depth/item/byte admission.
fn json(value: &Value, conversion: &mut Conversion) -> Result<Json, Error> {
    #[cfg(test)]
    {
        conversion.json_nodes += 1;
    }
    Ok(match value {
        Value::Bool(b) => Json::Bool(*b),
        Value::Text(text) => Json::String(conversion.string(text)),
        Value::Integer(i) => Json::from(i.0),
        Value::Unsigned(u) => Json::from(u.0),
        Value::Number(n) => Json::Number(serde_json::Number::from_f64(f64::from(*n)).ok_or_else(|| invalid("a parameter is not a finite number"))?),
        Value::List(items) => Json::Array(items.iter().map(|v| json(v, conversion)).collect::<Result<_, _>>()?),
        Value::Media(_) => return Err(unsupported("a media value can't be a model parameter")),
    })
}
struct MediaReads<'a> {
    source: Option<&'a dyn MediaSource>,
    left: u64,
}
fn inputs(role: &str, value: &Value, out: &mut Vec<JobInput>, media: &mut MediaReads<'_>, conversion: &mut Conversion) -> Result<(), Error> {
    if let Value::List(items) = value {
        for item in items {
            inputs(role, item, out, media, conversion)?;
        }
        return Ok(());
    }
    if let Value::Media(asset) = value {
        let source = media.source.ok_or_else(|| unsupported("this model call has no asset store to send media inputs from"))?;
        let bytes = source.read(asset, media.left)?;
        if asset.mime.essence() == nodecraft_media::style::MIME {
            style_matches(asset, &bytes)?;
        }
        // Admission summed the stored sizes; the bytes actually read are held to the same cap.
        media.left = u64::try_from(bytes.len())
            .ok()
            .and_then(|n| media.left.checked_sub(n))
            .ok_or_else(|| invalid("the model call's media inputs are larger than it allows"))?;
        let role = conversion.string(role);
        let mime = conversion.string(asset.mime.essence());
        #[cfg(test)]
        {
            conversion.input_records += 1;
            conversion.asset_arcs += 1;
        }
        out.push(JobInput::Media { role, mime, bytes });
        return Ok(());
    }
    let text = match value {
        Value::Text(text) => conversion.string(text),
        other => {
            let value = json(other, conversion)?;
            conversion.scalar_text(value)
        }
    };
    let role = conversion.string(role);
    #[cfg(test)]
    {
        conversion.input_records += 1;
    }
    out.push(JobInput::Text { role, text });
    Ok(())
}
fn request_with_limits(
    spec: &JobSpec,
    key: &str,
    limits: Limits,
    source: Option<&dyn MediaSource>,
    conversion: &mut Conversion,
) -> Result<(String, ProviderJob), Error> {
    let proof = admit_request(spec, key, limits, source)?;
    let JobSpec::Generate { provider, model, params, inputs: given, .. } = proof.spec else { return Err(invalid("not a model call")) };
    let mut map = Map::new();
    for (name, value) in params {
        let name = conversion.string(name);
        map.insert(name, json(value, conversion)?);
    }
    let mut job_inputs = Vec::new();
    let mut media = MediaReads { source, left: limits.media };
    for input in given {
        inputs(&input.role, &input.value, &mut job_inputs, &mut media, conversion)?;
    }
    Ok((
        conversion.string(provider),
        ProviderJob { model: conversion.string(model), params: map, inputs: job_inputs, idempotency_key: conversion.string(proof.key) },
    ))
}
/// Convert a standalone job after complete borrowed depth/item/text/representation admission.
/// The request representation budget is 64 MiB, with 4096 aggregate values/inputs and graph depth 32.
/// Text per original value is 1 MiB. Metadata and repeated flattened roles share the byte budget.
/// A job with media inputs is refused here; see [`provider_job_with_media`].
pub fn provider_job(spec: &JobSpec, idempotency_key: &str) -> Result<(String, ProviderJob), Error> {
    request_with_limits(spec, idempotency_key, Limits::default(), None, &mut Conversion::default())
}
/// [`provider_job`], reading media inputs from `source`. Their stored sizes are summed against
/// [`MAX_MEDIA_INPUT_BYTES`] before the first read, so an oversized call reads nothing.
pub fn provider_job_with_media(spec: &JobSpec, idempotency_key: &str, source: &dyn MediaSource) -> Result<(String, ProviderJob), Error> {
    request_with_limits(spec, idempotency_key, Limits::default(), Some(source), &mut Conversion::default())
}

/// Converted outputs and shared media bytes for the host to store; no asset store is mutated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Produced {
    pub outputs: Vec<(String, Value)>,
    pub assets: Vec<(Blake3, Arc<[u8]>)>,
}
/// A style asset's MIME type: the descriptor type with the family it was trained for.
pub fn style_mime(family: &str) -> String {
    format!("{}; family={family}", nodecraft_media::style::MIME)
}
/// A style about to be sent must be what its asset says: a valid descriptor of the family that
/// planning checked against the model (#167).
fn style_matches(asset: &AssetRef, bytes: &[u8]) -> Result<(), Error> {
    let style = nodecraft_media::style::decode(bytes).map_err(|_| invalid("a wired style is damaged; train it again"))?;
    if asset.mime.param("family") != Some(style.family.as_str()) {
        return Err(invalid("a wired style's file doesn't match the family its card records; train it again"));
    }
    Ok(())
}
fn modality(kind: MediaKind) -> Modality {
    match kind {
        MediaKind::Image => Modality::Image,
        MediaKind::Video => Modality::Video,
        MediaKind::Audio => Modality::Audio,
        MediaKind::Model3d => Modality::Mesh,
        MediaKind::Document => Modality::Document,
        MediaKind::Text => Modality::Text,
        MediaKind::Style => Modality::Style,
    }
}
fn index(kind: Modality) -> usize {
    match kind {
        Modality::Text => 0,
        Modality::Image => 1,
        Modality::Video => 2,
        Modality::Audio => 3,
        Modality::Mesh => 4,
        Modality::Document => 5,
        Modality::Style => 6,
    }
}
/// The first port for each modality, and the family a style port's trainer trains.
struct Routes<'a>([Option<&'a str>; 7], Option<&'a str>);
impl<'a> Routes<'a> {
    fn port(&self, kind: Modality) -> Result<&'a str, Error> {
        let label = match kind {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Video => "video",
            Modality::Audio => "audio",
            Modality::Mesh => "mesh",
            Modality::Document => "document",
            Modality::Style => "style",
        };
        self.0.get(index(kind)).copied().flatten().ok_or_else(|| invalid(&format!("the model returned {label} output but this node has no output port for it")))
    }
}
enum BorrowedOutput<'a> {
    Text { port: &'a str, text: &'a str },
    Media { port: &'a str, mime: std::borrow::Cow<'static, str>, bytes: &'a Arc<[u8]> },
}
#[derive(Default, Debug)]
struct ScanWork {
    #[cfg(test)]
    utf8_calls: usize,
    #[cfg(test)]
    utf8_bytes: usize,
}

impl ScanWork {
    // Test-only observer of actual UTF-8 scan attempts; production admission uses its own Budget.
    fn utf8_scan(&mut self, _bytes: usize) {
        #[cfg(test)]
        {
            self.utf8_calls += 1;
            self.utf8_bytes += _bytes;
        }
    }
}
fn raw_text_budget(text: usize, occurrences: usize) -> Result<Budget, Error> {
    let bytes = occurrences.checked_mul(3).and_then(|n| n.checked_add(text)).ok_or_else(|| invalid("provider raw text work limit overflowed"))?;
    Ok(Budget(bytes))
}
fn classify<'a>(item: &'a Output, routes: &Routes<'a>, limits: Limits, raw_text: &mut Budget, work: &mut ScanWork) -> Result<BorrowedOutput<'a>, Error> {
    match item {
        Output::Text(text) => Ok(BorrowedOutput::Text { port: routes.port(Modality::Text)?, text }),
        Output::Bytes { mime: claimed, bytes } => {
            // A typed SVG is not an arbitrary XML/text payload; validate its complete static
            // original before generic sniffing or any output/history publication.
            if claimed.split(';').next().is_some_and(|m| m.trim().eq_ignore_ascii_case(nodecraft_media::svg::MIME)) {
                nodecraft_media::svg::probe(bytes, &Default::default()).map_err(|_| invalid("the provider returned an unsafe or over-limit SVG"))?;
            }
            let mut sniffed = nodecraft_media::sniff(bytes).ok_or_else(|| invalid("the provider returned an output that isn't a known media type"))?;
            if sniffed.mime == nodecraft_media::svg::MIME {
                nodecraft_media::svg::probe(bytes, &Default::default()).map_err(|_| invalid("the provider returned an unsafe or over-limit SVG"))?;
            }
            // Text that merely starts like a style descriptor stays text where no style is expected.
            if sniffed.kind == MediaKind::Style && routes.0.get(index(Modality::Style)).copied().flatten().is_none() {
                sniffed = nodecraft_media::Sniffed { mime: "text/plain", kind: MediaKind::Text };
            }
            if matches!(sniffed.mime, "image/x-exr" | "image/vnd.radiance") {
                return Err(unsupported("HDR image outputs aren't supported yet; use PNG or JPEG"));
            }
            if sniffed.kind == MediaKind::Text {
                if !claimed.trim_start().get(..5).is_some_and(|head| head.eq_ignore_ascii_case("text/")) {
                    return Err(invalid("the provider claimed non-text output but returned text (an error page?)"));
                }
                if sniffed.mime != "text/plain" {
                    return Err(unsupported("the provider returned UTF-16 text, which isn't supported"));
                }
                // One standard UTF-8 BOM can precede an exact-limit payload. This raw-work
                // cap also applies to non-BOM/multiple-BOM data, before any full UTF-8 scan.
                let raw_limit = limits.text.checked_add(3).ok_or_else(|| invalid("provider raw text limit overflowed"))?;
                if bytes.len() > raw_limit {
                    return Err(invalid("the provider returned raw text over the text work limit"));
                }
                raw_text.charge(bytes.len()).map_err(|_| invalid("the provider's aggregate raw text exceeds the text work limit"))?;
                work.utf8_scan(bytes.len());
                let text = std::str::from_utf8(bytes).map_err(|_| invalid("the provider returned text that isn't UTF-8"))?;
                Ok(BorrowedOutput::Text { port: routes.port(Modality::Text)?, text: text.trim_start_matches('\u{feff}') })
            } else if sniffed.kind == MediaKind::Style {
                // A trained style (#167) is kept only once its descriptor checks out, and its asset
                // names the family it was trained for, so a card can refuse it before a run.
                let port = routes.port(Modality::Style)?;
                let style = nodecraft_media::style::decode(bytes).map_err(|error| invalid(&format!("the provider returned a damaged style: {error}")))?;
                // The catalog says which family a trainer trains; a style naming another is refused
                // rather than offered to models it was never trained for.
                if routes.1 != Some(style.family.as_str()) {
                    return Err(invalid("the provider returned a style for a different model family than its trainer trains"));
                }
                Ok(BorrowedOutput::Media { port, mime: style_mime(&style.family).into(), bytes })
            } else {
                Ok(BorrowedOutput::Media { port: routes.port(modality(sniffed.kind))?, mime: sniffed.mime.into(), bytes })
            }
        }
        Output::Url { .. } => Err(unsupported("download URL outputs before mapping them")),
    }
}
// Fixed cross-target logical charges, not allocator/RSS measurements.
const OUTPUT_PROOF_RECORD_BYTES: usize = 64;
const OUTPUT_PROOF_ROOT_BYTES: usize = 32;
const _: [(); 1] = [(); (std::mem::size_of::<BorrowedOutput<'static>>() <= OUTPUT_PROOF_RECORD_BYTES) as usize];
const _: [(); 1] = [(); (std::mem::size_of::<Vec<BorrowedOutput<'static>>>() <= OUTPUT_PROOF_ROOT_BYTES) as usize];
struct AdmittedOutputs<'a> {
    items: Vec<BorrowedOutput<'a>>,
}
fn admit_outputs<'a>(outputs: &'a [GenerateOutput], done: &'a Completed, limits: Limits, work: &mut ScanWork) -> Result<AdmittedOutputs<'a>, Error> {
    if done.outputs.is_empty() {
        return Err(invalid("the provider returned no output"));
    }
    if done.outputs.len() > limits.items || outputs.len() > limits.items {
        return Err(invalid("the provider result or route table has too many items"));
    }
    let mut budget = Budget(limits.outputs);
    let mut routes = Routes([None; 7], None);
    for output in outputs {
        budget.charge(64)?;
        budget.string(&output.port)?;
        if let Some(slot) = routes.0.get_mut(index(output.modality))
            && slot.is_none()
        {
            *slot = Some(output.port.as_str());
            if output.modality == Modality::Style {
                budget.string(output.style_family.as_deref().unwrap_or_default())?;
                routes.1 = output.style_family.as_deref();
            }
        }
    }
    // Cheap entire-result size/work admission before semantic scans; every Arc occurrence counts.
    let mut raw = Budget(limits.raw_outputs);
    for item in &done.outputs {
        budget.charge(128)?;
        match item {
            Output::Text(text) => {
                if text.len() > limits.text {
                    return Err(invalid("the provider returned text over the value text limit"));
                }
                raw.charge(text.len())?;
            }
            Output::Bytes { mime, bytes } => {
                if bytes.len() > limits.inline {
                    return Err(invalid("the provider returned an output too large to keep"));
                }
                raw.charge(bytes.len())?;
                budget.string(mime)?;
            }
            Output::Url { .. } => return Err(unsupported("download URL outputs before mapping them")),
        }
    }
    // Count actual occurrences, including aliases; the configured maximum is only a ceiling.
    // Direct strings/media do not need UTF-8 validation, but contribute at most three bytes
    // of bounded BOM allowance without an extra classification pass.
    let mut raw_text = raw_text_budget(limits.text, done.outputs.len())?;
    let proof_bytes = done
        .outputs
        .len()
        .checked_mul(OUTPUT_PROOF_RECORD_BYTES)
        .and_then(|n| n.checked_add(OUTPUT_PROOF_ROOT_BYTES))
        .ok_or_else(|| invalid("provider proof byte count overflowed"))?;
    budget.charge(proof_bytes)?;
    let mut items = Vec::new();
    items.try_reserve_exact(done.outputs.len()).map_err(|_| Error::new(ErrorKind::Busy, "provider output admission scratch couldn't be allocated"))?;
    let mut text_bytes = 0usize;
    // Bounded borrowed scratch is allowed here; no converted records, hashing or cloning
    // precedes complete semantic admission. Keep the validated &str instead of rescanning it.
    for item in &done.outputs {
        let classified = classify(item, &routes, limits, &mut raw_text, work)?;
        match &classified {
            BorrowedOutput::Text { port, text } => {
                text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| invalid("provider text byte count overflowed"))?;
                if text_bytes > limits.text {
                    return Err(invalid("the provider's routed text exceeds the value text limit"));
                }
                budget.string(port)?;
                budget.string(text)?;
            }
            BorrowedOutput::Media { port, mime, .. } => {
                budget.charge(512)?;
                budget.string(port)?;
                budget.string(mime)?;
            }
        }
        items.push(classified);
    }
    Ok(AdmittedOutputs { items })
}
fn outputs_with_limits(outputs: &[GenerateOutput], done: &Completed, limits: Limits, conversion: &mut Conversion) -> Result<Produced, Error> {
    outputs_with_work(outputs, done, limits, conversion, &mut ScanWork::default())
}
fn outputs_with_work(
    outputs: &[GenerateOutput],
    done: &Completed,
    limits: Limits,
    conversion: &mut Conversion,
    work: &mut ScanWork,
) -> Result<Produced, Error> {
    let proof = admit_outputs(outputs, done, limits, work)?;
    let mut produced = Produced::default();
    for item in proof.items {
        let (port, value) = match item {
            BorrowedOutput::Text { port, text } => (conversion.string(port), Value::Text(conversion.string(text))),
            BorrowedOutput::Media { port, mime, bytes } => {
                #[cfg(test)]
                {
                    conversion.hash_bytes += bytes.len();
                    conversion.asset_arcs += 1;
                }
                let hash = Blake3::from_bytes(ContentHash::of(bytes).0);
                // A generated sound or movie records the length its headers state (#608), so a
                // speech-to-video mode can check it against its limit before the next run.
                let meta = if mime == nodecraft_media::svg::MIME {
                    let info = nodecraft_media::svg::probe(bytes, &Default::default()).map_err(|_| invalid("the provider returned an unsafe SVG"))?;
                    nodecraft_types::MediaMeta { width: Some(info.width), height: Some(info.height), ..Default::default() }
                } else {
                    crate::import::timed_meta(&mime, bytes)
                };
                let mime = mime.parse().map_err(|_| Error::new(ErrorKind::Internal, "a sniffed media type didn't parse"))?;
                produced.assets.push((hash, Arc::clone(bytes)));
                (conversion.string(port), Value::Media(AssetRef { hash, mime, meta }))
            }
        };
        #[cfg(test)]
        {
            conversion.output_records += 1;
        }
        produced.outputs.push((port, value));
    }
    Ok(produced)
}
/// Map hostile results only after complete borrowed admission. Text totals at most 1 MiB;
/// binary items/raw aggregate at most 256 MiB; converted reference/string representation at most 8 MiB.
/// These are not full Generation cardinality/type or asset-persistence certification. URLs must
/// be downloaded first. Refusal leaves the original paid result with its caller.
pub fn produced(outputs: &[GenerateOutput], done: &Completed) -> Result<Produced, Error> {
    outputs_with_limits(outputs, done, Limits::default(), &mut Conversion::default())
}

#[cfg(test)]
mod tests;
