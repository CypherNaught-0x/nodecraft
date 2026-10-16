//! The WebSocket messages a ComfyUI server sends while it runs prompts, and a tracker that turns
//! them into progress for one prompt.

use std::collections::HashSet;

use serde_json::Value;

use crate::{ComfyError, FileRef};

/// Cap on one WebSocket message (previews are the large ones).
pub const MAX_WS_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// Image format of a binary preview frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewFormat {
    Jpeg,
    Png,
    Other(u32),
}

/// One message from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    /// Queue status; sent on connect and whenever the queue changes.
    Status {
        queue_remaining: Option<u64>,
        session_id: Option<String>,
    },
    ExecutionStart {
        prompt_id: String,
    },
    /// These nodes' results were reused from the server's cache and won't execute.
    ExecutionCached {
        prompt_id: String,
        nodes: Vec<String>,
    },
    /// `node` started executing; `None` means the prompt finished.
    Executing {
        prompt_id: Option<String>,
        node: Option<String>,
    },
    /// Step progress inside a node (sampler steps).
    Progress {
        prompt_id: Option<String>,
        node: Option<String>,
        value: u64,
        max: u64,
    },
    /// A node finished and produced these files (`output` is the raw result).
    Executed {
        prompt_id: String,
        node: String,
        files: Vec<FileRef>,
        output: Value,
    },
    ExecutionError {
        prompt_id: String,
        node: Option<String>,
        node_type: Option<String>,
        message: String,
        exception_type: Option<String>,
    },
    Interrupted {
        prompt_id: String,
        node: Option<String>,
    },
    Success {
        prompt_id: String,
    },
    /// A live preview image while sampling. Newer servers say which prompt and node it is for.
    Preview {
        format: PreviewFormat,
        bytes: Vec<u8>,
        prompt_id: Option<String>,
        node: Option<String>,
    },
    /// A message type this version doesn't know; ignore it.
    Other {
        kind: String,
    },
}

/// Parses a text (JSON) message.
pub fn parse_text(message: &str) -> Result<WsEvent, ComfyError> {
    let v: Value = crate::error::parse_json("WebSocket message", message.as_bytes(), MAX_WS_MESSAGE_BYTES)?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
    let empty = Value::Null;
    let data = v.get("data").unwrap_or(&empty);
    let text = |k: &str| data.get(k).and_then(id_or_text);
    let prompt_id = text("prompt_id");
    let need_prompt = || prompt_id.clone().ok_or_else(|| ComfyError::Malformed { what: "WebSocket message", message: format!("{kind} without prompt_id") });
    Ok(match kind.as_str() {
        "status" => WsEvent::Status {
            queue_remaining: data.get("status").and_then(|s| s.get("exec_info")).and_then(|e| e.get("queue_remaining")).and_then(Value::as_u64),
            session_id: text("sid"),
        },
        "execution_start" => WsEvent::ExecutionStart { prompt_id: need_prompt()? },
        "execution_cached" => WsEvent::ExecutionCached {
            prompt_id: need_prompt()?,
            nodes: data.get("nodes").and_then(Value::as_array).map(|a| a.iter().filter_map(id_or_text).collect()).unwrap_or_default(),
        },
        "executing" => WsEvent::Executing { prompt_id, node: text("node") },
        "progress" => WsEvent::Progress {
            prompt_id,
            node: text("node"),
            value: data.get("value").and_then(Value::as_u64).unwrap_or(0),
            max: data.get("max").and_then(Value::as_u64).unwrap_or(0),
        },
        "executed" => {
            let output = data.get("output").cloned().unwrap_or(Value::Null);
            WsEvent::Executed { prompt_id: need_prompt()?, node: text("node").unwrap_or_default(), files: files_in(&output), output }
        }
        "execution_error" => WsEvent::ExecutionError {
            prompt_id: need_prompt()?,
            node: text("node_id"),
            node_type: text("node_type"),
            message: text("exception_message").map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()).unwrap_or_else(|| "execution failed".into()),
            exception_type: text("exception_type"),
        },
        "execution_interrupted" => WsEvent::Interrupted { prompt_id: need_prompt()?, node: text("node_id") },
        "execution_success" => WsEvent::Success { prompt_id: need_prompt()? },
        _ => WsEvent::Other { kind },
    })
}

/// Parses a binary message: a big-endian `u32` event type, then
/// - 1 (preview image): a `u32` image format (1 = JPEG, 2 = PNG) and the image bytes;
/// - 4 (preview with metadata): a `u32` metadata length, that much JSON (`prompt_id`, `node_id`,
///   `image_type`) and the image bytes.
pub fn parse_binary(message: &[u8]) -> Result<WsEvent, ComfyError> {
    if message.len() > MAX_WS_MESSAGE_BYTES {
        return Err(ComfyError::TooLarge { what: "WebSocket message", size: message.len(), limit: MAX_WS_MESSAGE_BYTES });
    }
    let word = |at: usize| message.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_be_bytes);
    let malformed = || ComfyError::Malformed { what: "WebSocket message", message: "binary message too short".into() };
    let event = word(0).ok_or_else(malformed)?;
    match event {
        1 => {
            let format = match word(4).ok_or_else(malformed)? {
                1 => PreviewFormat::Jpeg,
                2 => PreviewFormat::Png,
                other => PreviewFormat::Other(other),
            };
            Ok(WsEvent::Preview { format, bytes: message.get(8..).unwrap_or_default().to_vec(), prompt_id: None, node: None })
        }
        4 => {
            let len = usize::try_from(word(4).ok_or_else(malformed)?).unwrap_or(usize::MAX);
            let end = len.checked_add(8).filter(|e| *e <= message.len()).ok_or_else(malformed)?;
            let meta: Value = message.get(8..end).and_then(|m| serde_json::from_slice(m).ok()).unwrap_or(Value::Null);
            let text = |k: &str| meta.get(k).and_then(id_or_text);
            let format = match meta.get("image_type").and_then(Value::as_str) {
                Some("image/png") => PreviewFormat::Png,
                Some("image/jpeg") => PreviewFormat::Jpeg,
                _ => PreviewFormat::Other(0),
            };
            Ok(WsEvent::Preview { format, bytes: message.get(end..).unwrap_or_default().to_vec(), prompt_id: text("prompt_id"), node: text("node_id") })
        }
        other => Ok(WsEvent::Other { kind: format!("binary:{other}") }),
    }
}

/// Node ids arrive as strings; tolerate numbers.
fn id_or_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Every file named in a node result: any list of objects with a `filename` (`images`, `gifs`,
/// `audio`, `video`, 3D outputs and whatever custom nodes use).
pub fn files_in(output: &Value) -> Vec<FileRef> {
    let Some(map) = output.as_object() else { return Vec::new() };
    let text = |item: &Value, k: &str| item.get(k).and_then(Value::as_str).map(str::to_owned);
    map.values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|item| {
            // `subfolder` and `type` may be missing or null.
            Some(FileRef {
                filename: text(item, "filename")?,
                subfolder: text(item, "subfolder").unwrap_or_default(),
                kind: text(item, "type").unwrap_or_else(|| "output".into()),
            })
        })
        .collect()
}

/// How a prompt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Error { node: Option<String>, node_type: Option<String>, message: String },
    Interrupted,
}

/// Progress of one queued prompt, fed with every [`WsEvent`]; events for other prompts are ignored.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptProgress {
    prompt_id: String,
    nodes: HashSet<String>,
    done: HashSet<String>,
    current: Option<String>,
    step: Option<(u64, u64)>,
    started: bool,
    outputs: Vec<(String, FileRef)>,
    outcome: Option<Outcome>,
}

impl PromptProgress {
    /// Tracks `prompt_id`, whose workflow has the nodes `node_ids` (see [`crate::ApiWorkflow::ids`]).
    /// Only those count towards progress: servers may also report cached or expanded nodes.
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(prompt_id: impl Into<String>, node_ids: I) -> Self {
        Self {
            prompt_id: prompt_id.into(),
            nodes: node_ids.into_iter().map(Into::into).collect(),
            done: HashSet::new(),
            current: None,
            step: None,
            started: false,
            outputs: Vec::new(),
            outcome: None,
        }
    }

    fn mine(&self, id: Option<&String>) -> bool {
        id.is_some_and(|id| *id == self.prompt_id)
    }

    /// Applies one event. Returns true when it was about this prompt.
    pub fn apply(&mut self, event: &WsEvent) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        match event {
            WsEvent::ExecutionStart { prompt_id } if self.mine(Some(prompt_id)) => self.started = true,
            WsEvent::ExecutionCached { prompt_id, nodes } if self.mine(Some(prompt_id)) => {
                self.started = true;
                self.done.extend(nodes.iter().cloned());
            }
            WsEvent::Executing { prompt_id, node } if self.mine(prompt_id.as_ref()) => {
                self.started = true;
                if let Some(previous) = self.current.take() {
                    self.done.insert(previous);
                }
                self.step = None;
                match node {
                    Some(node) => self.current = Some(node.clone()),
                    // Older servers end a prompt with `executing: null` and no `execution_success`.
                    None => self.outcome = Some(Outcome::Success),
                }
            }
            WsEvent::Progress { prompt_id, node, value, max }
                if self.mine(prompt_id.as_ref()) || (prompt_id.is_none() && self.current.is_some() && node == &self.current) =>
            {
                self.step = Some((*value, *max));
            }
            WsEvent::Executed { prompt_id, node, files, .. } if self.mine(Some(prompt_id)) => {
                self.done.insert(node.clone());
                self.outputs.extend(files.iter().map(|f| (node.clone(), f.clone())));
            }
            WsEvent::ExecutionError { prompt_id, node, node_type, message, .. } if self.mine(Some(prompt_id)) => {
                self.outcome = Some(Outcome::Error { node: node.clone(), node_type: node_type.clone(), message: message.clone() });
            }
            WsEvent::Interrupted { prompt_id, .. } if self.mine(Some(prompt_id)) => self.outcome = Some(Outcome::Interrupted),
            WsEvent::Success { prompt_id } if self.mine(Some(prompt_id)) => {
                if let Some(previous) = self.current.take() {
                    self.done.insert(previous);
                }
                self.outcome = Some(Outcome::Success);
            }
            _ => return false,
        }
        true
    }

    /// True once the server has started (or skipped through the cache) this prompt.
    pub fn started(&self) -> bool {
        self.started
    }

    /// The node running now, if any.
    pub fn current_node(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Overall progress from 0 to 1: finished nodes plus the step progress of the current one.
    pub fn fraction(&self) -> f32 {
        if matches!(self.outcome, Some(Outcome::Success)) {
            return 1.0;
        }
        if self.nodes.is_empty() {
            return 0.0;
        }
        let step = match self.step {
            Some((value, max)) if max > 0 => (value.min(max) as f64) / (max as f64),
            _ => 0.0,
        };
        let running = if self.current.is_some() { step } else { 0.0 };
        let done = self.done.iter().filter(|n| self.nodes.contains(*n)).count();
        let fraction = (done as f64 + running) / self.nodes.len() as f64;
        fraction.clamp(0.0, 1.0) as f32
    }

    /// Files the prompt produced so far, with the node that produced each.
    pub fn outputs(&self) -> &[(String, FileRef)] {
        &self.outputs
    }

    /// How the prompt ended, once it has.
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(json: &str) -> WsEvent {
        parse_text(json).unwrap()
    }

    #[test]
    fn parses_each_message_type() {
        assert_eq!(
            ev(r#"{"type":"status","data":{"status":{"exec_info":{"queue_remaining":2}},"sid":"s1"}}"#),
            WsEvent::Status { queue_remaining: Some(2), session_id: Some("s1".into()) }
        );
        assert_eq!(ev(r#"{"type":"execution_start","data":{"prompt_id":"p"}}"#), WsEvent::ExecutionStart { prompt_id: "p".into() });
        assert_eq!(
            ev(r#"{"type":"execution_cached","data":{"nodes":["4",5],"prompt_id":"p"}}"#),
            WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: vec!["4".into(), "5".into()] }
        );
        assert_eq!(ev(r#"{"type":"executing","data":{"node":null,"prompt_id":"p"}}"#), WsEvent::Executing { prompt_id: Some("p".into()), node: None });
        assert_eq!(
            ev(r#"{"type":"progress","data":{"value":3,"max":20,"prompt_id":"p","node":"3"}}"#),
            WsEvent::Progress { prompt_id: Some("p".into()), node: Some("3".into()), value: 3, max: 20 }
        );
        let executed = ev(
            r#"{"type":"executed","data":{"node":"9","prompt_id":"p","output":{"images":[{"filename":"NodeCraft_00001_.png","subfolder":"","type":"output"}],"text":["hi"]}}}"#,
        );
        let WsEvent::Executed { files, node, .. } = &executed else { panic!("{executed:?}") };
        assert_eq!(files_in(&serde_json::json!({"images":[{"filename":"a.png","subfolder":null,"type":null}]})).len(), 1, "null fields");
        assert_eq!(node, "9");
        assert_eq!(files, &[FileRef { filename: "NodeCraft_00001_.png".into(), subfolder: String::new(), kind: "output".into() }]);
        assert_eq!(
            ev(
                r#"{"type":"execution_error","data":{"prompt_id":"p","node_id":"3","node_type":"KSampler","exception_message":"CUDA out of memory\n","exception_type":"torch.OutOfMemoryError","traceback":[]}}"#
            ),
            WsEvent::ExecutionError {
                prompt_id: "p".into(),
                node: Some("3".into()),
                node_type: Some("KSampler".into()),
                message: "CUDA out of memory".into(),
                exception_type: Some("torch.OutOfMemoryError".into())
            }
        );
        assert_eq!(
            ev(r#"{"type":"execution_interrupted","data":{"prompt_id":"p","node_id":"3"}}"#),
            WsEvent::Interrupted { prompt_id: "p".into(), node: Some("3".into()) }
        );
        assert_eq!(ev(r#"{"type":"execution_success","data":{"prompt_id":"p"}}"#), WsEvent::Success { prompt_id: "p".into() });
        assert_eq!(ev(r#"{"type":"crystools.monitor","data":{}}"#), WsEvent::Other { kind: "crystools.monitor".into() });
    }

    #[test]
    fn malformed_messages_are_errors() {
        for bad in ["", "{", r#"{"type":"execution_start","data":{}}"#, r#"{"type":"executed"}"#] {
            assert!(parse_text(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_text("[]").unwrap(), WsEvent::Other { kind: String::new() });
    }

    #[test]
    fn binary_previews() {
        let mut frame = vec![0, 0, 0, 1, 0, 0, 0, 2];
        frame.extend_from_slice(b"\x89PNG");
        assert_eq!(parse_binary(&frame).unwrap(), WsEvent::Preview { format: PreviewFormat::Png, bytes: b"\x89PNG".to_vec(), prompt_id: None, node: None });
        assert_eq!(
            parse_binary(&[0, 0, 0, 1, 0, 0, 0, 1]).unwrap(),
            WsEvent::Preview { format: PreviewFormat::Jpeg, bytes: vec![], prompt_id: None, node: None }
        );
        assert_eq!(parse_binary(&[0, 0, 0, 3, 9]).unwrap(), WsEvent::Other { kind: "binary:3".into() });
        for short in [&[][..], &[0, 0, 0], &[0, 0, 0, 1, 0, 0], &[0, 0, 0, 4, 0, 0, 0, 99, b'{']] {
            assert!(parse_binary(short).is_err());
        }
        let meta = br#"{"prompt_id":"p","node_id":"3","display_node_id":"3","image_type":"image/png"}"#;
        let mut frame = vec![0, 0, 0, 4];
        frame.extend_from_slice(&u32::try_from(meta.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(meta);
        frame.extend_from_slice(b"IMG");
        assert_eq!(
            parse_binary(&frame).unwrap(),
            WsEvent::Preview { format: PreviewFormat::Png, bytes: b"IMG".to_vec(), prompt_id: Some("p".into()), node: Some("3".into()) }
        );
        let mut huge = vec![0, 0, 0, 4, 0xFF, 0xFF, 0xFF, 0xFF];
        huge.extend_from_slice(b"x");
        assert!(parse_binary(&huge).is_err());
    }

    #[test]
    fn tracks_one_prompt() {
        let mut p = PromptProgress::new("p", ["3", "4", "8", "9"]);
        assert!(!p.apply(&ev(r#"{"type":"execution_start","data":{"prompt_id":"other"}}"#)));
        assert!(p.apply(&ev(r#"{"type":"execution_cached","data":{"nodes":["4"],"prompt_id":"p"}}"#)));
        assert!(p.started());
        assert_eq!(p.fraction(), 0.25);
        p.apply(&ev(r#"{"type":"executing","data":{"node":"3","prompt_id":"p"}}"#));
        p.apply(&ev(r#"{"type":"progress","data":{"value":10,"max":20,"prompt_id":"p","node":"3"}}"#));
        assert_eq!(p.current_node(), Some("3"));
        assert!((p.fraction() - 0.375).abs() < 1e-6);
        p.apply(&ev(r#"{"type":"executing","data":{"node":"8","prompt_id":"p"}}"#));
        assert_eq!(p.fraction(), 0.5);
        p.apply(&ev(r#"{"type":"executed","data":{"node":"9","prompt_id":"p","output":{"images":[{"filename":"a.png","subfolder":"","type":"output"}]}}}"#));
        assert_eq!(p.outputs().len(), 1);
        p.apply(&ev(r#"{"type":"executing","data":{"node":null,"prompt_id":"p"}}"#));
        assert_eq!(p.outcome(), Some(&Outcome::Success));
        assert_eq!(p.fraction(), 1.0);
        assert!(!p.apply(&ev(r#"{"type":"execution_error","data":{"prompt_id":"p"}}"#)), "finished prompts ignore later events");
    }

    #[test]
    fn errors_and_interrupts_end_a_prompt() {
        let mut p = PromptProgress::new("p", ["1", "2"]);
        p.apply(&ev(r#"{"type":"execution_error","data":{"prompt_id":"p","node_id":"3","node_type":"KSampler","exception_message":"boom"}}"#));
        assert_eq!(p.outcome(), Some(&Outcome::Error { node: Some("3".into()), node_type: Some("KSampler".into()), message: "boom".into() }));
        let mut p = PromptProgress::new("p", ["1", "2"]);
        p.apply(&ev(r#"{"type":"execution_interrupted","data":{"prompt_id":"p"}}"#));
        assert_eq!(p.outcome(), Some(&Outcome::Interrupted));
    }

    #[test]
    fn hostile_progress_numbers() {
        let mut p = PromptProgress::new("p", ["1"]);
        p.apply(&WsEvent::Executing { prompt_id: Some("p".into()), node: Some("1".into()) });
        p.apply(&WsEvent::Progress { prompt_id: Some("p".into()), node: Some("1".into()), value: u64::MAX, max: 0 });
        assert_eq!(p.fraction(), 0.0);
        p.apply(&WsEvent::Progress { prompt_id: Some("p".into()), node: Some("1".into()), value: 50, max: 10 });
        assert_eq!(p.fraction(), 1.0);
        assert_eq!(PromptProgress::new("p", Vec::<String>::new()).fraction(), 0.0);
        let mut many = PromptProgress::new("p", ["1", "2"]);
        many.apply(&WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: (0..10).map(|n| format!("x{n}")).collect() });
        assert_eq!(many.fraction(), 0.0, "nodes outside the workflow don't count");
        many.apply(&WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: vec!["1".into()] });
        assert_eq!(many.fraction(), 0.5);
    }
}
