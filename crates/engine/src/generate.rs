//! Model calls (#79): turns a [`JobSpec::Generate`] into the request its provider adapter takes,
//! and the provider's [`Completed`] result back into values for the step's output ports.
//!
//! Parameters go out as plain JSON (numbers, text, booleans, lists). Text inputs go out as
//! role-tagged text; a list input sends one entry per item. Media inputs need the asset store,
//! which isn't wired yet, and are refused as unsupported rather than sent empty.
//!
//! Results are hostile input. Inline bytes are sniffed ([`check_media`]); the provider's claimed
//! type is ignored. Each result goes to the first output port whose modality it has, and a result
//! no port takes fails the job. Text comes back as a text value. Media comes back as a value that
//! references the bytes by hash; the host stores them in its asset store under that hash.

use nodecraft_graph::MAX_VALUE_ITEMS;
use nodecraft_media::MediaKind;
use nodecraft_nodes::{GenerateOutput, JobSpec};
use nodecraft_providers::{Completed, JobInput, Output, ProviderJob, check_media};
use nodecraft_types::{AssetRef, Blake3, Error, ErrorKind, MediaMeta, Modality, Value};
use serde_json::{Map, Value as Json};
use std::sync::Arc;

/// Largest inline result accepted, in bytes.
pub const MAX_INLINE_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

fn unsupported(message: &str) -> Error {
    Error::new(ErrorKind::Unsupported, message.to_owned())
}

/// A parameter value as plain JSON.
fn json(value: &Value) -> Result<Json, Error> {
    // Lists nest only as deep as the document allows, so this recursion is bounded.
    Ok(match value {
        Value::Bool(b) => Json::Bool(*b),
        Value::Text(t) => Json::String(t.clone()),
        Value::Integer(i) => Json::from(i.0),
        Value::Unsigned(u) => Json::from(u.0),
        Value::Number(n) => {
            Json::Number(serde_json::Number::from_f64(f64::from(*n)).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "a parameter is not a finite number"))?)
        }
        Value::List(items) => Json::Array(items.iter().map(json).collect::<Result<_, _>>()?),
        Value::Media(_) => return Err(unsupported("a media value can't be a model parameter")),
    })
}

/// Adds `value` as inputs in `role`: text as one entry, lists item by item, within the cap.
fn inputs(role: &str, value: &Value, out: &mut Vec<JobInput>) -> Result<(), Error> {
    if out.len() >= MAX_VALUE_ITEMS {
        return Err(Error::new(ErrorKind::InvalidInput, "the model call has too many inputs"));
    }
    match value {
        Value::Text(text) => out.push(JobInput::Text { role: role.to_owned(), text: text.clone() }),
        Value::List(items) => {
            for item in items {
                inputs(role, item, out)?;
            }
        }
        Value::Media(_) => return Err(unsupported("media inputs need the asset store, which isn't available yet")),
        other => out.push(JobInput::Text { role: role.to_owned(), text: json(other)?.to_string() }),
    }
    Ok(())
}

/// The provider id and request for a generate job, sent under `idempotency_key`.
pub fn provider_job(spec: &JobSpec, idempotency_key: &str) -> Result<(String, ProviderJob), Error> {
    let JobSpec::Generate { provider, model, params, inputs: given, .. } = spec else {
        return Err(Error::new(ErrorKind::InvalidInput, "not a model call"));
    };
    let mut map = Map::new();
    for (name, value) in params {
        map.insert(name.clone(), json(value)?);
    }
    let mut job_inputs = Vec::new();
    for input in given {
        inputs(&input.role, &input.value, &mut job_inputs)?;
    }
    Ok((provider.clone(), ProviderJob { model: model.clone(), params: map, inputs: job_inputs, idempotency_key: idempotency_key.to_owned() }))
}

/// What a finished model call produced: `(port, value)` outputs for the run executor, and the
/// bytes of media outputs, by hash, for the host to store.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Produced {
    pub outputs: Vec<(String, Value)>,
    pub assets: Vec<(Blake3, Arc<[u8]>)>,
}

fn modality_of(kind: MediaKind) -> Modality {
    match kind {
        MediaKind::Image => Modality::Image,
        MediaKind::Video => Modality::Video,
        MediaKind::Audio => Modality::Audio,
        MediaKind::Model3d => Modality::Mesh,
        MediaKind::Document => Modality::Document,
        MediaKind::Text => Modality::Text,
    }
}

fn port_for(outputs: &[GenerateOutput], modality: Modality) -> Result<&str, Error> {
    outputs
        .iter()
        .find(|o| o.modality == modality)
        .map(|o| o.port.as_str())
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("the model returned {modality:?} output, which this node has no output for")))
}

/// Routes a provider's result onto `outputs` (see the module docs). URLs must be downloaded
/// first (`fetch_output`).
pub fn produced(outputs: &[GenerateOutput], done: &Completed) -> Result<Produced, Error> {
    if done.outputs.len() > MAX_VALUE_ITEMS {
        return Err(Error::new(ErrorKind::InvalidInput, "the provider returned too many outputs"));
    }
    let mut produced = Produced::default();
    for item in &done.outputs {
        match item {
            Output::Text(text) => produced.outputs.push((port_for(outputs, Modality::Text)?.to_owned(), Value::Text(text.clone()))),
            Output::Bytes { bytes, .. } => {
                if bytes.len() > MAX_INLINE_OUTPUT_BYTES {
                    return Err(Error::new(ErrorKind::InvalidInput, "the provider returned an output too large to keep"));
                }
                let checked = check_media(bytes, None).map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
                let modality = modality_of(nodecraft_media::sniff(bytes).map_or(MediaKind::Text, |s| s.kind));
                let port = port_for(outputs, modality)?.to_owned();
                if modality == Modality::Text {
                    let text =
                        String::from_utf8(checked.bytes).map_err(|_| Error::new(ErrorKind::InvalidInput, "the provider returned text that isn't UTF-8"))?;
                    produced.outputs.push((port, Value::Text(text)));
                    continue;
                }
                let hash = Blake3::from_bytes(checked.hash.0);
                let mime = checked.mime.parse().map_err(|_| Error::new(ErrorKind::Internal, "a sniffed media type didn't parse"))?;
                produced.assets.push((hash, Arc::clone(bytes)));
                produced.outputs.push((port, Value::Media(AssetRef { hash, mime, meta: MediaMeta::default() })));
            }
            Output::Url { .. } => return Err(unsupported("download URL outputs before mapping them")),
        }
    }
    Ok(produced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_nodes::GenerateInput;
    use nodecraft_types::{Integer, NodeId, Number};

    fn routes() -> Vec<GenerateOutput> {
        vec![GenerateOutput { port: "image".into(), modality: Modality::Image }, GenerateOutput { port: "caption".into(), modality: Modality::Text }]
    }

    fn spec(inputs: Vec<GenerateInput>) -> JobSpec {
        let params = [
            ("steps".to_owned(), Value::Integer(Integer(20))),
            ("cfg".to_owned(), Value::Number(Number::new(4.5).unwrap())),
            ("hd".to_owned(), Value::Bool(true)),
            ("style".to_owned(), Value::Text("ink".into())),
        ]
        .into_iter()
        .collect();
        JobSpec::Generate { node: NodeId::from_bytes([1; 16]), outputs: routes(), provider: "mock".into(), model: "mock-image".into(), params, inputs }
    }

    #[test]
    fn a_generate_job_becomes_a_provider_request() {
        let given = vec![
            GenerateInput { role: "prompt".into(), value: Value::Text("a fox".into()) },
            GenerateInput { role: "reference".into(), value: Value::List(vec![Value::Text("one".into()), Value::Text("two".into())]) },
        ];
        let (provider, job) = provider_job(&spec(given), "nodecraft:r:1").unwrap();
        assert_eq!(provider, "mock");
        assert_eq!(job.model, "mock-image");
        assert_eq!(job.idempotency_key, "nodecraft:r:1");
        assert_eq!(Json::Object(job.params), serde_json::json!({"steps": 20, "cfg": 4.5, "hd": true, "style": "ink"}));
        assert_eq!(job.inputs.len(), 3);
        assert_eq!(job.inputs[0], JobInput::Text { role: "prompt".into(), text: "a fox".into() });
        // Media inputs and source jobs are refused.
        let media = Value::Media(AssetRef { hash: Blake3::from_bytes([2; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
        assert_eq!(provider_job(&spec(vec![GenerateInput { role: "image".into(), value: media }]), "k").unwrap_err().kind, ErrorKind::Unsupported);
        let source = JobSpec::Source { node: NodeId::from_bytes([1; 16]), output: "out".into(), value: Value::Text("x".into()) };
        assert!(provider_job(&source, "k").is_err());
        // More inputs than a list can hold are refused while flattening.
        let many = Value::List((0..=MAX_VALUE_ITEMS).map(|n| Value::Text(n.to_string())).collect());
        assert!(provider_job(&spec(vec![GenerateInput { role: "prompt".into(), value: many }]), "k").is_err());
    }

    #[test]
    fn results_are_sniffed_and_routed_by_modality() {
        let png: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\n0123456789"[..]);
        // The claimed type is ignored: the bytes are a PNG.
        let done = Completed::new(vec![Output::Text("a fox".into()), Output::Bytes { mime: "application/octet-stream".into(), bytes: Arc::clone(&png) }]);
        let got = produced(&routes(), &done).unwrap();
        assert_eq!(got.outputs[0], ("caption".to_owned(), Value::Text("a fox".into())));
        let (port, Value::Media(asset)) = &got.outputs[1] else { panic!("not media") };
        assert_eq!((port.as_str(), asset.mime.as_str()), ("image", "image/png"));
        assert_eq!(got.assets, vec![(asset.hash, png)]);
        // An HTML error page claiming to be a PNG isn't media; text bytes go to the text port.
        let html = Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"<html>error</html>"[..]) }]);
        assert_eq!(produced(&routes(), &html).unwrap().outputs, vec![("caption".to_owned(), Value::Text("<html>error</html>".into()))]);
        let image_only = [GenerateOutput { port: "image".into(), modality: Modality::Image }];
        assert!(produced(&image_only, &html).is_err(), "no port takes text");
        // Unknown binary, audio on an image-only node, and URLs are refused.
        let binary = Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"\x00\x01\x02\x03"[..]) }]);
        assert!(produced(&routes(), &binary).is_err());
        let wav = Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"RIFF\x00\x00\x00\x00WAVEfmt "[..]) }]);
        assert!(produced(&image_only, &wav).is_err());
        assert!(produced(&routes(), &Completed::new(vec![Output::Url { url: "https://x".into(), mime: None }])).is_err());
    }
}
