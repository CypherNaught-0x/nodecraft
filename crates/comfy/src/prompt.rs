//! Queueing a workflow: the `POST /prompt` body and the server's reply.

use serde_json::{Map, Value};

use crate::error::NodeError;
use crate::{ApiWorkflow, ComfyError};

/// Cap on a `/prompt` reply (validation errors list every node).
const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;

/// A workflow to queue, addressed to one WebSocket client so its progress comes back to us.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRequest {
    pub workflow: ApiWorkflow,
    /// The `clientId` the WebSocket was opened with ([`crate::Server::websocket_url`]).
    pub client_id: String,
    /// Passed through as `extra_data` (e.g. `{"extra_pnginfo": …}`).
    pub extra_data: Map<String, Value>,
}

impl PromptRequest {
    pub fn new(workflow: ApiWorkflow, client_id: impl Into<String>) -> Self {
        Self { workflow, client_id: client_id.into(), extra_data: Map::new() }
    }

    pub fn to_json(&self) -> Value {
        let mut body = Map::new();
        body.insert("prompt".into(), self.workflow.to_json());
        body.insert("client_id".into(), Value::String(self.client_id.clone()));
        if !self.extra_data.is_empty() {
            body.insert("extra_data".into(), Value::Object(self.extra_data.clone()));
        }
        Value::Object(body)
    }
}

/// A prompt the server accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub prompt_id: String,
    /// Position number in the server's queue.
    pub number: u64,
    /// Problems in nodes that don't feed any output; the prompt runs anyway.
    pub node_errors: Vec<NodeError>,
}

/// Reads the reply to `POST /prompt` with its HTTP status.
pub fn parse_prompt_response(status: u16, bytes: &[u8]) -> Result<Queued, ComfyError> {
    let parsed: Result<Value, _> = crate::error::parse_json("/prompt reply", bytes, MAX_REPLY_BYTES);
    let ok = (200..300).contains(&status);
    let v = match parsed {
        Ok(v) => v,
        Err(e) if ok => return Err(e),
        Err(_) => {
            let text = String::from_utf8_lossy(bytes.get(..bytes.len().min(300)).unwrap_or_default()).trim().to_owned();
            let message = if text.is_empty() { "no details".into() } else { text };
            return Err(ComfyError::Server { status, message, node_errors: Vec::new() });
        }
    };
    let node_errors = parse_node_errors(v.get("node_errors"));
    if ok && let Some(prompt_id) = v.get("prompt_id").and_then(Value::as_str) {
        let number = v.get("number").and_then(Value::as_u64).unwrap_or(0);
        return Ok(Queued { prompt_id: prompt_id.to_owned(), number, node_errors });
    }
    let error = v.get("error");
    let message = match error {
        Some(Value::String(s)) => s.clone(),
        Some(e) => {
            let msg = e.get("message").and_then(Value::as_str).unwrap_or("request refused");
            match e.get("details").and_then(Value::as_str).filter(|d| !d.is_empty()) {
                Some(details) => format!("{msg}: {details}"),
                None => msg.to_owned(),
            }
        }
        None if ok => "the reply has no prompt_id".into(),
        None => "request refused".into(),
    };
    Err(ComfyError::Server { status, message, node_errors })
}

fn parse_node_errors(v: Option<&Value>) -> Vec<NodeError> {
    let Some(Value::Object(map)) = v else { return Vec::new() };
    let mut ids: Vec<&String> = map.keys().collect();
    ids.sort_by(|a, b| crate::workflow::id_order(a, b));
    let mut out = Vec::new();
    for id in ids {
        let Some(entry) = map.get(id) else { continue };
        let class_type = entry.get("class_type").and_then(Value::as_str).unwrap_or_default().to_owned();
        for e in entry.get("errors").and_then(Value::as_array).into_iter().flatten() {
            let message = e.get("message").and_then(Value::as_str).unwrap_or("error").to_owned();
            let details = e.get("details").and_then(Value::as_str).filter(|d| !d.is_empty());
            out.push(NodeError {
                node: id.clone(),
                class_type: class_type.clone(),
                message: details.map_or_else(|| message.clone(), |d| format!("{message}: {d}")),
                input: e.get("extra_info").and_then(|x| x.get("input_name")).and_then(Value::as_str).map(str::to_owned),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[test]
    fn request_body() {
        let wf = ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap();
        let mut req = PromptRequest::new(wf.clone(), "client-1");
        let body = req.to_json();
        assert_eq!(body["client_id"], "client-1");
        assert_eq!(body["prompt"], wf.to_json());
        assert!(body.get("extra_data").is_none());
        req.extra_data.insert("k".into(), Value::Bool(true));
        assert_eq!(req.to_json()["extra_data"]["k"], true);
        let http = crate::Server::new("http://h:8188").unwrap().prompt(&req).unwrap();
        assert_eq!(http.url.as_str(), "http://h:8188/prompt");
    }

    #[test]
    fn accepted() {
        let q = parse_prompt_response(200, br#"{"prompt_id":"abc","number":7,"node_errors":{}}"#).unwrap();
        assert_eq!(q, Queued { prompt_id: "abc".into(), number: 7, node_errors: vec![] });
    }

    #[test]
    fn validation_errors_name_nodes_and_inputs() {
        let reply = br#"{"error":{"type":"prompt_outputs_failed_validation","message":"Prompt outputs failed validation","details":"","extra_info":{}},
          "node_errors":{"10":{"errors":[{"type":"value_not_in_list","message":"Value not in list","details":"ckpt_name: 'x' not in [...]","extra_info":{"input_name":"ckpt_name"}}],"dependent_outputs":["9"],"class_type":"CheckpointLoaderSimple"},
                         "3":{"errors":[{"type":"value_smaller_than_min","message":"Value 0 smaller than min of 1","details":"","extra_info":{"input_name":"steps"}}],"class_type":"KSampler"}}}"#;
        let err = parse_prompt_response(400, reply).unwrap_err();
        let ComfyError::Server { status, message, node_errors } = &err else { panic!("{err:?}") };
        assert_eq!(*status, 400);
        assert_eq!(message, "Prompt outputs failed validation");
        assert_eq!(node_errors.len(), 2);
        assert_eq!(node_errors[0].node, "3", "numeric id order");
        assert_eq!(node_errors[0].input.as_deref(), Some("steps"));
        assert_eq!(node_errors[1].message, "Value not in list: ckpt_name: 'x' not in [...]");
        assert!(err.to_string().contains("node 10 (CheckpointLoaderSimple) input ckpt_name"));
    }

    #[test]
    fn odd_replies() {
        let e = parse_prompt_response(502, b"<html>Bad Gateway</html>").unwrap_err();
        assert!(matches!(e, ComfyError::Server { status: 502, ref message, .. } if message.contains("Bad Gateway")));
        assert!(matches!(parse_prompt_response(500, b""), Err(ComfyError::Server { .. })));
        assert!(matches!(parse_prompt_response(200, b"{}"), Err(ComfyError::Server { .. })));
        assert!(matches!(parse_prompt_response(200, b"nope"), Err(ComfyError::Malformed { .. })));
        assert!(matches!(parse_prompt_response(400, br#"{"error":"bad"}"#), Err(ComfyError::Server { ref message, .. }) if message == "bad"));
        let _ = parse_prompt_response(400, br#"{"node_errors":{"1":{"errors":[5,{"extra_info":3}]},"2":7}}"#);
    }
}
