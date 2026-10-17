//! Model calls (#79): turns a [`JobSpec::Generate`] into the request its provider adapter takes,
//! and the provider's [`Completed`] result back into values for the step's output port.
//!
//! Parameters go out as plain JSON (numbers, text, booleans, lists). Text inputs go out as
//! role-tagged text; a list input sends one entry per item. Media inputs need the asset store,
//! which isn't wired yet, and are refused as unsupported rather than sent empty. Results come back
//! as text values, or media values referencing the returned bytes by hash: the host stores those
//! bytes in its asset store under that hash.

use nodecraft_graph::MAX_VALUE_ITEMS;
use nodecraft_nodes::JobSpec;
use nodecraft_providers::{Completed, JobInput, Output, ProviderJob};
use nodecraft_types::{AssetRef, Blake3, Error, ErrorKind, MediaMeta, Value};
use serde_json::{Map, Value as Json};
use std::sync::Arc;

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
        // Numbers are finite by construction; a non-finite one would become null.
        Value::Number(n) => serde_json::Number::from_f64(f64::from(*n)).map_or(Json::Null, Json::Number),
        Value::List(items) => Json::Array(items.iter().map(json).collect::<Result<_, _>>()?),
        Value::Media(_) => return Err(unsupported("a media value can't be a model parameter")),
    })
}

/// Adds `value` as inputs in `role`: text as one entry, lists item by item.
fn inputs(role: &str, value: &Value, out: &mut Vec<JobInput>) -> Result<(), Error> {
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
        if job_inputs.len() > MAX_VALUE_ITEMS {
            return Err(Error::new(ErrorKind::InvalidInput, "the model call has too many inputs"));
        }
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

/// Maps a provider's result onto `output`. URLs must be downloaded first (`fetch_output`).
pub fn produced(output: &str, done: &Completed) -> Result<Produced, Error> {
    if done.outputs.len() > MAX_VALUE_ITEMS {
        return Err(Error::new(ErrorKind::InvalidInput, "the provider returned too many outputs"));
    }
    let mut produced = Produced::default();
    for item in &done.outputs {
        let value = match item {
            Output::Text(text) => Value::Text(text.clone()),
            Output::Bytes { mime, bytes } => {
                let hash = Blake3::from_bytes(*blake3::hash(bytes).as_bytes());
                let mime = mime.parse().map_err(|_| Error::new(ErrorKind::InvalidInput, "the provider returned media with an invalid type"))?;
                produced.assets.push((hash, Arc::clone(bytes)));
                Value::Media(AssetRef { hash, mime, meta: MediaMeta::default() })
            }
            Output::Url { .. } => return Err(unsupported("download URL outputs before mapping them")),
        };
        produced.outputs.push((output.to_owned(), value));
    }
    Ok(produced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_nodes::GenerateInput;
    use nodecraft_types::{Integer, NodeId, Number};

    fn spec(inputs: Vec<GenerateInput>) -> JobSpec {
        let params = [
            ("steps".to_owned(), Value::Integer(Integer(20))),
            ("cfg".to_owned(), Value::Number(Number::new(4.5).unwrap())),
            ("hd".to_owned(), Value::Bool(true)),
            ("style".to_owned(), Value::Text("ink".into())),
        ]
        .into_iter()
        .collect();
        JobSpec::Generate { node: NodeId::from_bytes([1; 16]), output: "out".into(), provider: "mock".into(), model: "mock-image".into(), params, inputs }
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
    }

    #[test]
    fn results_map_to_output_values_and_assets() {
        let bytes: Arc<[u8]> = Arc::from(&b"\x89PNG..."[..]);
        let done = Completed::new(vec![Output::Text("hello".into()), Output::Bytes { mime: "image/png".into(), bytes: Arc::clone(&bytes) }]);
        let produced = produced("out", &done).unwrap();
        assert_eq!(produced.outputs[0], ("out".to_owned(), Value::Text("hello".into())));
        let Value::Media(asset) = &produced.outputs[1].1 else { panic!("not media") };
        assert_eq!(asset.hash, Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()));
        assert_eq!(produced.assets, vec![(asset.hash, bytes)]);
        // URLs need downloading first; a bad media type is refused.
        assert!(produced_err(Output::Url { url: "https://x".into(), mime: None }));
        assert!(produced_err(Output::Bytes { mime: "not a type".into(), bytes: Arc::from(&b""[..]) }));
    }

    fn produced_err(output: Output) -> bool {
        produced("out", &Completed::new(vec![output])).is_err()
    }
}
