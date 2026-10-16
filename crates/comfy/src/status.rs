//! Server health and state: `/system_stats`, `/queue` and `/history`.

use serde_json::Value;

use crate::{ComfyError, FileRef};

const MAX_STATUS_BYTES: usize = 4 * 1024 * 1024;
/// `/history` replies echo the whole prompt back, so they can be large.
const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;

/// A GPU (or CPU/MPS device) the server runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    /// `cuda`, `mps`, `cpu`, …
    pub kind: String,
    pub vram_total: u64,
    pub vram_free: u64,
}

/// What `/system_stats` reports; also the health check (a reply means the server is up).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemStats {
    pub os: String,
    pub comfyui_version: Option<String>,
    pub python_version: Option<String>,
    pub ram_total: u64,
    pub ram_free: u64,
    pub devices: Vec<Device>,
}

impl SystemStats {
    pub fn parse(bytes: &[u8]) -> Result<Self, ComfyError> {
        let v: Value = crate::error::parse_json("/system_stats", bytes, MAX_STATUS_BYTES)?;
        let system = v.get("system").ok_or_else(|| ComfyError::Malformed { what: "/system_stats", message: "no system section".into() })?;
        let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let devices = v
            .get("devices")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|d| Device {
                        name: text(d, "name").unwrap_or_default(),
                        kind: text(d, "type").unwrap_or_default(),
                        vram_total: num(d, "vram_total"),
                        vram_free: num(d, "vram_free"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            os: text(system, "os").unwrap_or_default(),
            comfyui_version: text(system, "comfyui_version"),
            python_version: text(system, "python_version"),
            ram_total: num(system, "ram_total"),
            ram_free: num(system, "ram_free"),
            devices,
        })
    }

    /// One line for the servers panel, e.g. `NVIDIA GeForce RTX 4090 · 18.2 / 24.0 GB free`.
    pub fn summary(&self) -> String {
        let gb = |b: u64| b as f64 / 1e9;
        match self.devices.first() {
            Some(d) if d.vram_total > 0 => format!("{} · {:.1} / {:.1} GB free", d.name, gb(d.vram_free), gb(d.vram_total)),
            Some(d) => d.name.clone(),
            None => "no device reported".into(),
        }
    }
}

/// Prompts the server is running and waiting to run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueState {
    pub running: Vec<String>,
    pub pending: Vec<String>,
}

impl QueueState {
    pub fn parse(bytes: &[u8]) -> Result<Self, ComfyError> {
        let v: Value = crate::error::parse_json("/queue", bytes, MAX_HISTORY_BYTES)?;
        // Each entry is `[number, prompt_id, prompt, extra_data, outputs]`.
        let ids = |k: &str| -> Vec<String> {
            v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|e| e.get(1)?.as_str().map(str::to_owned)).collect()).unwrap_or_default()
        };
        Ok(Self { running: ids("queue_running"), pending: ids("queue_pending") })
    }

    pub fn len(&self) -> usize {
        self.running.len().saturating_add(self.pending.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, prompt_id: &str) -> bool {
        self.running.iter().chain(&self.pending).any(|p| p == prompt_id)
    }
}

/// A finished (or failed) prompt from `/history/{prompt_id}`; the fallback when WebSocket
/// messages were missed, e.g. after a reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub prompt_id: String,
    pub completed: bool,
    /// `success` or `error` as the server reports it.
    pub status: String,
    /// The prompt was interrupted (the server reports that as an error too).
    pub interrupted: bool,
    /// Output files by node, in numeric node order.
    pub outputs: Vec<(String, Vec<FileRef>)>,
    /// The error message when the prompt failed.
    pub error: Option<String>,
    /// The node that failed, and its class.
    pub error_node: Option<(String, String)>,
}

impl HistoryEntry {
    /// Reads `/history/{prompt_id}`. `Ok(None)` means the server has no record yet (still queued
    /// or running).
    pub fn parse(bytes: &[u8], prompt_id: &str) -> Result<Option<Self>, ComfyError> {
        let v: Value = crate::error::parse_json("/history", bytes, MAX_HISTORY_BYTES)?;
        let Some(entry) = v.get(prompt_id) else { return Ok(None) };
        let status = entry.get("status");
        let mut outputs: Vec<(String, Vec<FileRef>)> = entry
            .get("outputs")
            .and_then(Value::as_object)
            .map(|o| o.iter().map(|(node, out)| (node.clone(), crate::ws::files_in(out))).collect())
            .unwrap_or_default();
        outputs.sort_by(|a, b| crate::workflow::id_order(&a.0, &b.0));
        let messages: Vec<&Value> = status.and_then(|s| s.get("messages")).and_then(Value::as_array).into_iter().flatten().collect();
        let message = |kind: &str| messages.iter().find(|m| m.get(0).and_then(Value::as_str) == Some(kind)).and_then(|m| m.get(1));
        let failure = message("execution_error");
        let error = failure.and_then(|d| d.get("exception_message")?.as_str().map(|s| s.trim().to_owned()));
        let text = |d: &Value, k: &str| d.get(k).and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())));
        let error_node = failure.and_then(|d| Some((text(d, "node_id")?, text(d, "node_type").unwrap_or_default())));
        let interrupted = message("execution_interrupted").is_some();
        Ok(Some(Self {
            prompt_id: prompt_id.to_owned(),
            completed: status.and_then(|s| s.get("completed")).and_then(Value::as_bool).unwrap_or(false),
            status: status.and_then(|s| s.get("status_str")).and_then(Value::as_str).unwrap_or_default().to_owned(),
            interrupted,
            outputs,
            error,
            error_node,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_stats() {
        let s = SystemStats::parse(
            br#"{"system":{"os":"posix","ram_total":68719476736,"ram_free":34359738368,"comfyui_version":"0.3.40","python_version":"3.12.3","embedded_python":false},
                 "devices":[{"name":"cuda:0 NVIDIA GeForce RTX 4090 : cudaMallocAsync","type":"cuda","index":0,"vram_total":25757220864,"vram_free":18200000000}]}"#,
        )
        .unwrap();
        assert_eq!(s.os, "posix");
        assert_eq!(s.comfyui_version.as_deref(), Some("0.3.40"));
        assert_eq!(s.devices[0].kind, "cuda");
        assert_eq!(s.summary(), "cuda:0 NVIDIA GeForce RTX 4090 : cudaMallocAsync · 18.2 / 25.8 GB free");
        assert_eq!(SystemStats::parse(br#"{"system":{}}"#).unwrap().summary(), "no device reported");
        assert!(SystemStats::parse(b"{}").is_err());
        assert!(SystemStats::parse(b"<html>").is_err());
    }

    #[test]
    fn queue() {
        let q = QueueState::parse(br#"{"queue_running":[[3,"a",{},{},["9"]]],"queue_pending":[[4,"b",{},{},[]],[5,"c"],[6],"junk"]}"#).unwrap();
        assert_eq!(q.running, ["a"]);
        assert_eq!(q.pending, ["b", "c"]);
        assert_eq!(q.len(), 3);
        assert!(q.contains("c") && !q.contains("z"));
        assert!(QueueState::parse(b"{}").unwrap().is_empty());
    }

    #[test]
    fn history() {
        let body = br#"{"p1":{"prompt":[1,"p1",{},{},["9"]],
            "outputs":{"9":{"images":[{"filename":"NodeCraft_00001_.png","subfolder":"","type":"output"}]},"12":{"gifs":[{"filename":"clip.mp4","subfolder":"video","type":"output","format":"video/h264-mp4"}]}},
            "status":{"status_str":"success","completed":true,"messages":[["execution_start",{"prompt_id":"p1"}]]}}}"#;
        let h = HistoryEntry::parse(body, "p1").unwrap().unwrap();
        assert!(h.completed);
        assert_eq!(h.status, "success");
        assert_eq!(h.outputs.iter().map(|(n, f)| (n.as_str(), f.len())).collect::<Vec<_>>(), [("9", 1), ("12", 1)]);
        assert_eq!(h.outputs[1].1[0].subfolder, "video");
        assert!(HistoryEntry::parse(body, "other").unwrap().is_none());

        let failed = br#"{"p2":{"outputs":{},"status":{"status_str":"error","completed":false,
            "messages":[["execution_error",{"prompt_id":"p2","node_id":"3","exception_message":"CUDA out of memory\n"}]]}}}"#;
        let h = HistoryEntry::parse(failed, "p2").unwrap().unwrap();
        assert!(!h.completed);
        assert_eq!(h.error.as_deref(), Some("CUDA out of memory"));
        assert_eq!(h.error_node, Some(("3".into(), String::new())));
        assert!(!h.interrupted);
        let stopped = br#"{"p3":{"outputs":{},"status":{"status_str":"error","completed":false,"messages":[["execution_interrupted",{"prompt_id":"p3","node_id":"3","node_type":"KSampler"}]]}}}"#;
        assert!(HistoryEntry::parse(stopped, "p3").unwrap().unwrap().interrupted);
        assert!(HistoryEntry::parse(b"[1]", "p").unwrap().is_none());
        assert!(HistoryEntry::parse(br#"{"p":{"outputs":5,"status":[]}}"#, "p").unwrap().is_some());
    }
}
