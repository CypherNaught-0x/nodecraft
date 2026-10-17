//! The WebSocket messages a ComfyUI server sends while it runs prompts, and a tracker that turns
//! them into progress for one prompt.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::progress_bounds::{FileBudget, Limits, association_bytes, exceeded, sum};
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
    let mut v: Value = crate::error::parse_json("WebSocket message", message.as_bytes(), MAX_WS_MESSAGE_BYTES)?;
    // Check cached-ID extraction before copying any event fields. The raw JSON byte cap
    // does not bound the allocated Vec/String representation of many small identifiers.
    if v.get("type").and_then(Value::as_str) == Some("execution_cached") {
        if let Some(nodes) = v.get("data").and_then(|data| data.get("nodes")).and_then(Value::as_array) {
            let mut budget = FileBudget::default();
            budget.scan(nodes.len())?;
            for node in nodes {
                match node {
                    Value::String(node) => budget.charge(node.len().saturating_add(64))?,
                    Value::Number(_) => budget.charge(96)?,
                    _ => {}
                }
            }
        }
    }
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
            let prompt_id = need_prompt()?;
            let node = text("node").unwrap_or_default();
            let files = files_in(data.get("output").unwrap_or(&empty))?;
            let output = v.get_mut("data").and_then(Value::as_object_mut).and_then(|data| data.remove("output")).unwrap_or(Value::Null);
            WsEvent::Executed { prompt_id, node, files, output }
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
/// `audio`, `video`, 3D outputs and whatever custom nodes use). Enumeration is shallow and
/// fallible: shared file-count, scanned-entry and metadata-storage limits apply before copying.
pub fn files_in(output: &Value) -> Result<Vec<FileRef>, ComfyError> {
    files_in_with(output, &mut FileBudget::default())
}

pub(crate) fn files_in_with(output: &Value, budget: &mut FileBudget) -> Result<Vec<FileRef>, ComfyError> {
    let Some(map) = output.as_object() else { return Ok(Vec::new()) };
    budget.scan(map.len())?;
    let mut files = Vec::new();
    for value in map.values() {
        let Some(items) = value.as_array() else { continue };
        budget.scan(items.len())?;
        for item in items {
            let Some(filename) = item.get("filename").and_then(Value::as_str) else { continue };
            let subfolder = item.get("subfolder").and_then(Value::as_str).unwrap_or_default();
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("output");
            budget.file(sum(&[filename.len(), subfolder.len(), kind.len()]))?;
            files.push(FileRef { filename: filename.into(), subfolder: subfolder.into(), kind: kind.into() });
        }
    }
    Ok(files)
}

/// How a prompt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// Tracking stopped at a metadata limit; retained outputs are an incomplete prefix.
    /// This allocation-free terminal failure cannot be overwritten by later success events.
    MetadataLimit,
    Error {
        node: Option<String>,
        node_type: Option<String>,
        message: String,
    },
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
    seen_outputs: HashMap<String, HashSet<FileRef>>,
    retained_bytes: usize,
    limits: Limits,
    outcome: Option<Outcome>,
}

impl PromptProgress {
    /// Tracks `prompt_id`, whose workflow has the nodes `node_ids` (see [`crate::ApiWorkflow::ids`]).
    /// Only those count towards progress: servers may also report cached or expanded nodes.
    /// Identifiers, count and conservative retained storage are checked before copying; see
    /// [`crate::MAX_PROGRESS_NODES`] and [`crate::MAX_RETAINED_METADATA_BYTES`].
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<str>>(prompt_id: impl AsRef<str>, node_ids: I) -> Result<Self, ComfyError> {
        Self::with_limits(prompt_id.as_ref(), node_ids, Limits::default())
    }

    fn with_limits<I: IntoIterator<Item = S>, S: AsRef<str>>(prompt_id: &str, node_ids: I, limits: Limits) -> Result<Self, ComfyError> {
        let mut retained_bytes = 512usize.saturating_add(prompt_id.len());
        if retained_bytes > limits.bytes {
            return Err(exceeded());
        }
        let mut nodes = HashSet::new();
        for (index, node) in node_ids.into_iter().enumerate() {
            if index >= limits.scanned {
                return Err(exceeded());
            }
            let node = node.as_ref();
            if node.len() > limits.bytes {
                return Err(exceeded());
            }
            if nodes.contains(node) {
                continue;
            }
            if nodes.len() >= limits.nodes {
                return Err(exceeded());
            }
            // Reserve space for both the workflow set and its eventual completed-ID set.
            retained_bytes = retained_bytes.saturating_add(256usize.saturating_add(node.len().saturating_mul(2)));
            if retained_bytes > limits.bytes {
                return Err(exceeded());
            }
            nodes.insert(node.to_owned());
        }
        Ok(Self {
            prompt_id: prompt_id.into(),
            nodes,
            done: HashSet::new(),
            current: None,
            step: None,
            started: false,
            outputs: Vec::new(),
            seen_outputs: HashMap::new(),
            retained_bytes,
            limits,
            outcome: None,
        })
    }

    fn finish_node(&mut self, node: String) {
        if self.nodes.contains(&node) {
            self.done.insert(node);
        }
    }

    fn replacing_current(&self, node: Option<&str>) -> Result<usize, ComfyError> {
        let previous = self.current.as_ref().map_or(0, |node| 128usize.saturating_add(node.len()));
        let next = node.map_or(0, |node| 128usize.saturating_add(node.len()));
        let bytes = self.retained_bytes.saturating_sub(previous).saturating_add(next);
        if bytes > self.limits.bytes {
            return Err(exceeded());
        }
        Ok(bytes)
    }

    fn mine(&self, id: Option<&String>) -> bool {
        id.is_some_and(|id| *id == self.prompt_id)
    }

    /// Applies one event. Returns true when it was about this prompt. A limit error preserves
    /// the admitted progress/output prefix and latches [`Outcome::MetadataLimit`], so subsequent
    /// success cannot hide missing metadata. Unrelated prompts and completed trackers are ignored.
    pub fn apply(&mut self, event: &WsEvent) -> Result<bool, ComfyError> {
        let result = self.admit(event);
        if result.is_err() {
            self.outcome = Some(Outcome::MetadataLimit);
        }
        result
    }

    fn admit(&mut self, event: &WsEvent) -> Result<bool, ComfyError> {
        if self.outcome.is_some() {
            return Ok(false);
        }
        match event {
            WsEvent::ExecutionStart { prompt_id } if self.mine(Some(prompt_id)) => self.started = true,
            WsEvent::ExecutionCached { prompt_id, nodes } if self.mine(Some(prompt_id)) => {
                if nodes.len() > self.limits.scanned || nodes.iter().any(|node| node.len() > self.limits.bytes) {
                    return Err(exceeded());
                }
                self.started = true;
                self.done.extend(nodes.iter().filter(|node| self.nodes.contains(*node)).cloned());
            }
            WsEvent::Executing { prompt_id, node } if self.mine(prompt_id.as_ref()) => {
                let retained_bytes = self.replacing_current(node.as_deref())?;
                self.started = true;
                if let Some(previous) = self.current.take() {
                    self.finish_node(previous);
                }
                self.retained_bytes = retained_bytes;
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
                if files.len() > self.limits.scanned || node.len() > self.limits.bytes {
                    return Err(exceeded());
                }
                let mut bytes = self.retained_bytes;
                let mut pending = Vec::new();
                let mut seen = HashSet::new();
                for file in files {
                    let cost = association_bytes(node, file);
                    if cost > self.limits.bytes {
                        return Err(exceeded());
                    }
                    if self.seen_outputs.get(node).is_some_and(|known| known.contains(file)) || !seen.insert(file) {
                        continue;
                    }
                    if self.outputs.len().saturating_add(pending.len()) >= self.limits.files {
                        return Err(exceeded());
                    }
                    bytes = bytes.saturating_add(cost);
                    if bytes > self.limits.bytes {
                        return Err(exceeded());
                    }
                    pending.push(file);
                }
                if self.nodes.contains(node) {
                    self.done.insert(node.clone());
                }
                for file in pending {
                    self.seen_outputs.entry(node.clone()).or_default().insert(file.clone());
                    self.outputs.push((node.clone(), file.clone()));
                }
                self.retained_bytes = bytes;
            }
            WsEvent::ExecutionError { prompt_id, node, node_type, message, .. } if self.mine(Some(prompt_id)) => {
                let bytes = self.retained_bytes.saturating_add(sum(&[
                    128,
                    node.as_ref().map_or(0, String::len),
                    node_type.as_ref().map_or(0, String::len),
                    message.len(),
                ]));
                if bytes > self.limits.bytes {
                    return Err(exceeded());
                }
                self.outcome = Some(Outcome::Error { node: node.clone(), node_type: node_type.clone(), message: message.clone() });
                self.retained_bytes = bytes;
            }
            WsEvent::Interrupted { prompt_id, .. } if self.mine(Some(prompt_id)) => self.outcome = Some(Outcome::Interrupted),
            WsEvent::Success { prompt_id } if self.mine(Some(prompt_id)) => {
                self.retained_bytes = self.replacing_current(None)?;
                if let Some(previous) = self.current.take() {
                    self.finish_node(previous);
                }
                self.outcome = Some(Outcome::Success);
            }
            _ => return Ok(false),
        }
        Ok(true)
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
        let running = if self.current.as_ref().is_some_and(|node| self.nodes.contains(node)) { step } else { 0.0 };
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
        assert_eq!(files_in(&serde_json::json!({"images":[{"filename":"a.png","subfolder":null,"type":null}]})).unwrap().len(), 1, "null fields");
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
        let mut p = PromptProgress::new("p", ["3", "4", "8", "9"]).unwrap();
        assert!(!p.apply(&ev(r#"{"type":"execution_start","data":{"prompt_id":"other"}}"#)).unwrap());
        assert!(p.apply(&ev(r#"{"type":"execution_cached","data":{"nodes":["4"],"prompt_id":"p"}}"#)).unwrap());
        assert!(p.started());
        assert_eq!(p.fraction(), 0.25);
        p.apply(&ev(r#"{"type":"executing","data":{"node":"3","prompt_id":"p"}}"#)).unwrap();
        p.apply(&ev(r#"{"type":"progress","data":{"value":10,"max":20,"prompt_id":"p","node":"3"}}"#)).unwrap();
        assert_eq!(p.current_node(), Some("3"));
        assert!((p.fraction() - 0.375).abs() < 1e-6);
        p.apply(&ev(r#"{"type":"executing","data":{"node":"8","prompt_id":"p"}}"#)).unwrap();
        assert_eq!(p.fraction(), 0.5);
        p.apply(&ev(r#"{"type":"executed","data":{"node":"9","prompt_id":"p","output":{"images":[{"filename":"a.png","subfolder":"","type":"output"}]}}}"#))
            .unwrap();
        assert_eq!(p.outputs().len(), 1);
        p.apply(&ev(r#"{"type":"executing","data":{"node":null,"prompt_id":"p"}}"#)).unwrap();
        assert_eq!(p.outcome(), Some(&Outcome::Success));
        assert_eq!(p.fraction(), 1.0);
        assert!(!p.apply(&ev(r#"{"type":"execution_error","data":{"prompt_id":"p"}}"#)).unwrap(), "finished prompts ignore later events");
    }

    #[test]
    fn errors_and_interrupts_end_a_prompt() {
        let mut p = PromptProgress::new("p", ["1", "2"]).unwrap();
        p.apply(&ev(r#"{"type":"execution_error","data":{"prompt_id":"p","node_id":"3","node_type":"KSampler","exception_message":"boom"}}"#)).unwrap();
        assert_eq!(p.outcome(), Some(&Outcome::Error { node: Some("3".into()), node_type: Some("KSampler".into()), message: "boom".into() }));
        let mut p = PromptProgress::new("p", ["1", "2"]).unwrap();
        p.apply(&ev(r#"{"type":"execution_interrupted","data":{"prompt_id":"p"}}"#)).unwrap();
        assert_eq!(p.outcome(), Some(&Outcome::Interrupted));
    }

    #[test]
    fn hostile_progress_numbers() {
        let mut p = PromptProgress::new("p", ["1"]).unwrap();
        p.apply(&WsEvent::Executing { prompt_id: Some("p".into()), node: Some("1".into()) }).unwrap();
        p.apply(&WsEvent::Progress { prompt_id: Some("p".into()), node: Some("1".into()), value: u64::MAX, max: 0 }).unwrap();
        assert_eq!(p.fraction(), 0.0);
        p.apply(&WsEvent::Progress { prompt_id: Some("p".into()), node: Some("1".into()), value: 50, max: 10 }).unwrap();
        assert_eq!(p.fraction(), 1.0);
        assert_eq!(PromptProgress::new("p", Vec::<String>::new()).unwrap().fraction(), 0.0);
        let mut many = PromptProgress::new("p", ["1", "2"]).unwrap();
        many.apply(&WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: (0..10).map(|n| format!("x{n}")).collect() }).unwrap();
        assert_eq!(many.fraction(), 0.0, "nodes outside the workflow don't count");
        many.apply(&WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: vec!["1".into()] }).unwrap();
        assert_eq!(many.fraction(), 0.5);
    }
    fn output(node: &str, name: &str) -> WsEvent {
        WsEvent::Executed {
            prompt_id: "p".into(),
            node: node.into(),
            files: vec![FileRef { filename: name.into(), subfolder: String::new(), kind: "output".into() }],
            output: Value::Null,
        }
    }

    #[test]
    fn long_expanded_node_streams_do_not_accumulate_completed_ids() {
        let mut progress = PromptProgress::new("p", ["known"]).unwrap();
        for id in 0..20_000 {
            let node = format!("expanded{id}");
            progress.apply(&WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: vec![node.clone()] }).unwrap();
            progress.apply(&WsEvent::Executing { prompt_id: Some("p".into()), node: Some(node.clone()) }).unwrap();
            progress.apply(&WsEvent::Progress { prompt_id: Some("p".into()), node: Some(node), value: 1, max: 1 }).unwrap();
        }
        assert!(progress.done.is_empty());
        assert_eq!(progress.fraction(), 0.0);
        assert!(progress.retained_bytes < 2048);
        progress.apply(&WsEvent::Success { prompt_id: "p".into() }).unwrap();
        assert!(progress.done.is_empty());
    }

    #[test]
    fn repeated_outputs_are_idempotent_but_distinct_node_file_associations_survive() {
        let mut progress = PromptProgress::new("p", ["1", "2"]).unwrap();
        progress.apply(&output("1", "a.png")).unwrap();
        let bytes = progress.retained_bytes;
        for _ in 0..20_000 {
            progress.apply(&output("1", "a.png")).unwrap();
        }
        assert_eq!(progress.outputs.len(), 1);
        assert_eq!(progress.retained_bytes, bytes);
        progress.apply(&output("2", "a.png")).unwrap();
        progress.apply(&output("1", "b.png")).unwrap();
        assert_eq!(progress.outputs.len(), 3);
        assert_eq!(progress.done.len(), 2);
    }

    #[test]
    fn constructor_bounds_identifiers_count_and_even_duplicate_iterator_work() {
        let limits = Limits { nodes: 1, bytes: 800, scanned: 2, ..Limits::default() };
        assert!(PromptProgress::with_limits("p", ["1"], limits).is_ok());
        assert!(PromptProgress::with_limits("p", ["1", "2"], limits).is_err());
        assert!(PromptProgress::with_limits("p", ["1", "1", "1"], limits).is_err());
        assert!(PromptProgress::with_limits(&"x".repeat(800), ["1"], limits).is_err());
        assert!(PromptProgress::with_limits("p", ["x".repeat(800)], limits).is_err());
        let bytes = PromptProgress::new("p", ["1"]).unwrap().retained_bytes;
        assert!(PromptProgress::with_limits("p", ["1"], Limits { bytes, ..limits }).is_ok());
        assert!(PromptProgress::with_limits("p", ["1"], Limits { bytes: bytes - 1, ..limits }).is_err());
    }

    fn assert_limit_preserves_prefix(progress: &PromptProgress, before: &PromptProgress) {
        let mut expected = before.clone();
        expected.outcome = Some(Outcome::MetadataLimit);
        assert_eq!(progress, &expected);
    }

    #[test]
    fn output_count_and_byte_limits_preserve_prefix_and_latch_terminal_failure() {
        let limits = Limits { files: 1, bytes: 4096, ..Limits::default() };
        let mut progress = PromptProgress::with_limits("p", ["1", "2"], limits).unwrap();
        let mut event = output("1", "a.png");
        let WsEvent::Executed { files, .. } = &mut event else { panic!("executed") };
        files.push(FileRef { filename: "b.png".into(), subfolder: String::new(), kind: "output".into() });
        let before = progress.clone();
        assert!(progress.apply(&event).is_err());
        assert_limit_preserves_prefix(&progress, &before);
        progress = before.clone();
        progress.apply(&output("1", "a.png")).unwrap();
        let before = progress.clone();
        assert!(progress.apply(&output("2", "b.png")).is_err());
        assert_limit_preserves_prefix(&progress, &before);
        progress = before.clone();
        progress.apply(&output("1", "a.png")).unwrap();
        let mut progress = PromptProgress::new("p", ["1"]).unwrap();
        let file = FileRef { filename: "a.png".into(), subfolder: String::new(), kind: "output".into() };
        let bytes = progress.retained_bytes + association_bytes("1", &file);
        progress.limits.bytes = bytes;
        progress.apply(&output("1", "a.png")).unwrap();
        assert_eq!(progress.retained_bytes, bytes);
        let before = progress.clone();
        assert!(progress.apply(&output("1", "b.png")).is_err());
        assert_limit_preserves_prefix(&progress, &before);
    }

    #[test]
    fn oversized_programmatic_event_metadata_is_rejected_before_copies_and_errors_are_safe() {
        let mut progress = PromptProgress::with_limits("p", ["1"], Limits { bytes: 2048, ..Limits::default() }).unwrap();
        let secret = "fabricated-private-metadata".repeat(100);
        for event in [
            WsEvent::Executing { prompt_id: Some("p".into()), node: Some(secret.clone()) },
            output("1", &secret),
            WsEvent::ExecutionCached { prompt_id: "p".into(), nodes: vec![secret.clone()] },
            WsEvent::ExecutionError { prompt_id: "p".into(), node: Some(secret.clone()), node_type: None, message: "failure".into(), exception_type: None },
            WsEvent::ExecutionError { prompt_id: "p".into(), node: None, node_type: None, message: secret.clone(), exception_type: None },
        ] {
            let mut progress = progress.clone();
            let before = progress.clone();
            let error = progress.apply(&event).unwrap_err();
            assert!(!error.to_string().contains("fabricated-private-metadata"));
            assert_limit_preserves_prefix(&progress, &before);
        }
        let other = WsEvent::Executing { prompt_id: Some("other".into()), node: Some(secret) };
        assert_eq!(progress.apply(&other), Ok(false));
        progress.apply(&WsEvent::Executing { prompt_id: Some("p".into()), node: None }).unwrap();
        assert_eq!(progress.outcome(), Some(&Outcome::Success));
        assert_eq!(progress.apply(&output("1", "late.png")), Ok(false));
    }

    #[test]
    fn file_extraction_is_shallow_bounded_and_fallible() {
        let repeated = serde_json::json!({"images":vec![serde_json::json!({"filename":"a.png"});crate::MAX_OUTPUT_FILES+1]});
        assert!(files_in(&repeated).is_err());
        let oversized = serde_json::json!({"images":[{"filename":"x".repeat(crate::MAX_RETAINED_METADATA_BYTES)}]});
        assert!(files_in(&oversized).is_err());
        let many_ignored = serde_json::json!({"ignored":vec![Value::Null;crate::MAX_OUTPUT_ITEMS]});
        assert!(files_in(&many_ignored).is_err());
        let mut nested = Value::Null;
        for _ in 0..50_000 {
            nested = Value::Array(vec![nested]);
        }
        let mut output = serde_json::json!({"images":[{"filename":"a.png"}], "ignored":null});
        output.as_object_mut().unwrap().insert("ignored".into(), nested);
        assert_eq!(files_in(&output).unwrap().len(), 1);
        // The borrowed extractor never recursively clones the unrelated data. The caller owns
        // destruction of programmatically built deep Values.
        let mut nested = output.as_object_mut().unwrap().remove("ignored").unwrap();
        while let Value::Array(mut values) = nested {
            nested = values.pop().unwrap_or(Value::Null);
        }
    }
    #[test]
    fn rejected_output_admission_cannot_be_hidden_by_later_success() {
        for terminal in [WsEvent::Success { prompt_id: "p".into() }, WsEvent::Executing { prompt_id: Some("p".into()), node: None }] {
            let mut progress = PromptProgress::with_limits("p", ["1"], Limits { files: 1, ..Limits::default() }).unwrap();
            progress.apply(&output("1", "retained.png")).unwrap();
            assert!(progress.apply(&output("1", "rejected.png")).is_err());
            progress.apply(&terminal).unwrap();
            assert_eq!(progress.outcome(), Some(&Outcome::MetadataLimit), "missing metadata cannot become clean success");
            assert_eq!(progress.outputs().len(), 1);
        }
    }

    #[test]
    fn oversized_terminal_error_is_a_bounded_terminal_failure() {
        let mut progress = PromptProgress::with_limits("p", ["1"], Limits { bytes: 1024, ..Limits::default() }).unwrap();
        let error = WsEvent::ExecutionError { prompt_id: "p".into(), node: None, node_type: None, message: "private-error".repeat(100), exception_type: None };
        let before = progress.clone();
        assert!(progress.apply(&error).is_err());
        assert_limit_preserves_prefix(&progress, &before);
        assert_eq!(progress.outcome(), Some(&Outcome::MetadataLimit));
        assert_eq!(progress.apply(&WsEvent::Success { prompt_id: "p".into() }), Ok(false));
    }

    #[test]
    fn cached_parser_bounds_count_and_allocated_metadata_before_copying() {
        let message =
            serde_json::to_string(&serde_json::json!({"type":"execution_cached","data":{"prompt_id":"p","nodes":vec!["1";crate::MAX_OUTPUT_ITEMS+1]}}))
                .unwrap();
        assert!(message.len() < MAX_WS_MESSAGE_BYTES);
        assert!(parse_text(&message).is_err());
        let message =
            serde_json::to_string(&serde_json::json!({"type":"execution_cached","data":{"prompt_id":"p","nodes":vec!["x".repeat(100);60_000]}})).unwrap();
        assert!(message.len() < MAX_WS_MESSAGE_BYTES);
        assert!(parse_text(&message).is_err(), "allocated identifier overhead has its own budget");
        assert!(
            matches!(parse_text(r#"{"type":"execution_cached","data":{"prompt_id":"p","nodes":["1",2,null]}}"#).unwrap(), WsEvent::ExecutionCached { nodes, .. } if nodes == ["1", "2"])
        );
    }
}
