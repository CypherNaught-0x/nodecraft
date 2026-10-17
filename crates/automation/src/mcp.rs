//! The MCP server: NodeCraft's engine commands as Model Context Protocol tools.
//!
//! [`McpServer`] is sans-IO: [`McpServer::handle`] takes one JSON-RPC message and returns the
//! reply, if any. [`serve`] runs it over newline-delimited JSON (the MCP stdio transport).
//!
//! The tools are generic, so they track the command registry without a second list:
//! - `command_list`: the commands an agent may run, with their parameter schemas and whether each
//!   is enabled right now;
//! - `command_run`: runs one command through the same `Session::dispatch` as the UI;
//! - `canvas_inspect`: the project's canvases, or one canvas's nodes and edges, paged;
//! - `ids_new`: fresh ids for commands that take caller-supplied ids (`node.add`).
//!
//! Commands in [`HUMAN_ONLY`] (setting or removing API keys) are never listed or run here.

use std::io::{BufRead, Write};

use nodecraft_engine::Session;
use nodecraft_engine::commands::CommandSpec;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::{Map, Value, json};

/// Protocol revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest message accepted, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// Most nodes `canvas_inspect` returns per page.
pub const MAX_PAGE: usize = 500;
/// Most ids `ids_new` hands out per call.
pub const MAX_NEW_IDS: usize = 100;
/// Commands only a person may run: one takes a secret, which must not pass through an agent, and
/// one deletes a key with no undo.
pub const HUMAN_ONLY: &[&str] = &["keys.set", "keys.remove"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Where fresh ids come from (randomness belongs to the host platform).
pub type IdSource = Box<dyn FnMut() -> [u8; 16]>;

/// One MCP connection over one engine session.
pub struct McpServer {
    session: Session,
    new_id: IdSource,
    initialized: bool,
}

/// Whether automation may see and run `spec`.
pub fn visible(spec: &CommandSpec) -> bool {
    !HUMAN_ONLY.contains(&spec.id)
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A tool result: the JSON as structured content, and as text for clients that only read text.
fn tool_ok(value: Value) -> Value {
    let text = value.to_string();
    let structured = if value.is_object() { value } else { json!({"result": value}) };
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured, "isError": false})
}

/// A tool that ran and failed: reported to the model (which can correct itself), not as a
/// protocol error.
fn tool_err(e: &Error) -> Value {
    let kind = serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| format!("{:?}", e.kind));
    json!({"content": [{"type": "text", "text": format!("{kind}: {}", e.message)}], "isError": true})
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn tools() -> Value {
    json!([
        {
            "name": "command_list",
            "title": "List commands",
            "description": "Lists the NodeCraft commands you can run with command_run: id, label, parameter schema and whether it can run right now (with the reason when not). Filter with `query` (matches id or label).",
            "inputSchema": object_schema(json!({"query": {"type": "string", "maxLength": 200}}), &[]),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "command_run",
            "title": "Run a command",
            "description": "Runs one NodeCraft command, exactly as the app's UI would. `params` must match the command's schema from command_list. Document edits can be undone with edit.undo.",
            "inputSchema": object_schema(json!({"id": {"type": "string", "maxLength": 128}, "params": {"type": "object"}}), &["id"]),
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false}
        },
        {
            "name": "canvas_inspect",
            "title": "Inspect the project",
            "description": "Without `canvas`: the project and its canvases with node and edge counts. With `canvas`: that canvas's nodes (paged by `offset` and `limit`, sorted by id) and the edges touching them.",
            "inputSchema": object_schema(json!({
                "canvas": {"type": "string", "description": "Canvas id (UUID)"},
                "offset": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE}
            }), &[]),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "ids_new",
            "title": "New ids",
            "description": "Fresh unique ids (UUIDs) for commands that take a caller-chosen id, such as node.add.",
            "inputSchema": object_schema(json!({"count": {"type": "integer", "minimum": 1, "maximum": MAX_NEW_IDS}}), &[]),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }
    ])
}

impl McpServer {
    pub fn new(session: Session, new_id: IdSource) -> Self {
        Self { session, new_id, initialized: false }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Handles one message (a JSON-RPC request or notification) and returns the reply to send,
    /// or `None` for notifications and replies.
    pub fn handle(&mut self, message: &str) -> Option<String> {
        if message.len() > MAX_MESSAGE_BYTES {
            return Some(rpc_error(Value::Null, INVALID_REQUEST, format!("message is larger than {MAX_MESSAGE_BYTES} bytes")).to_string());
        }
        let parsed: Value = match serde_json::from_str(message) {
            Ok(v) => v,
            Err(_) => return Some(rpc_error(Value::Null, PARSE_ERROR, "not valid JSON").to_string()),
        };
        self.handle_value(parsed).map(|v| v.to_string())
    }

    fn handle_value(&mut self, message: Value) -> Option<Value> {
        let Some(obj) = message.as_object() else {
            // JSON-RPC batches were removed from MCP in 2025-06-18.
            return Some(rpc_error(Value::Null, INVALID_REQUEST, "expected a single JSON-RPC message object"));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response to a request we never sent, or garbage: answer requests, ignore the rest.
            return match id {
                Some(id) if !obj.contains_key("result") && !obj.contains_key("error") => Some(rpc_error(id, INVALID_REQUEST, "missing method")),
                _ => None,
            };
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Object(Map::new()));
        let Some(id) = id else {
            // Notifications: `notifications/initialized`, `notifications/cancelled`, …
            if method == "notifications/initialized" {
                self.initialized = true;
            }
            return None;
        };
        if !(id.is_string() || id.is_i64() || id.is_u64()) {
            return Some(rpc_error(Value::Null, INVALID_REQUEST, "the request id must be a string or an integer"));
        }
        let reply = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => self.call(&params),
            other => Err((METHOD_NOT_FOUND, format!("unknown method {}", other.chars().take(64).collect::<String>()))),
        };
        Some(match reply {
            Ok(result) => rpc_result(id, result),
            Err((code, message)) => rpc_error(id, code, message),
        })
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
        let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).or(PROTOCOL_VERSIONS.first()).copied().unwrap_or_default();
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "nodecraft", "title": "NodeCraft", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "NodeCraft is a node canvas for generative media. Call command_list to see what you can do, canvas_inspect to read the project, ids_new for new node ids, and command_run to change things (edits can be undone with edit.undo)."
        })
    }

    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "tools/call needs a tool name".to_owned()))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err((INVALID_PARAMS, "tool arguments must be an object".into())),
        };
        let result = match name {
            "command_list" => self.command_list(&args),
            "command_run" => self.command_run(&args),
            "canvas_inspect" => self.canvas_inspect(&args),
            "ids_new" => self.ids_new(&args),
            other => return Err((INVALID_PARAMS, format!("unknown tool {}", other.chars().take(64).collect::<String>()))),
        };
        Ok(match result {
            Ok(v) => tool_ok(v),
            Err(e) => tool_err(&e),
        })
    }

    fn command_list(&self, args: &Value) -> Result<Value, Error> {
        let query = args.get("query").and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let commands: Vec<Value> = self
            .session
            .registry()
            .specs()
            .filter(|s| visible(s))
            .filter(|s| query.is_empty() || s.id.contains(&query) || s.label.to_lowercase().contains(&query))
            .map(|s| {
                // `enabled` is checked with empty params: commands that need params say why.
                let enabled = self.session.enabled(s.id, &json!({}));
                json!({
                    "id": s.id,
                    "label": s.label,
                    "params_schema": (s.input_schema)(),
                    "params_doc": s.params_doc,
                    "ready": enabled.is_ok(),
                    "not_ready_because": enabled.err().map(|e| e.message),
                })
            })
            .collect();
        Ok(json!({"commands": commands}))
    }

    fn command_run(&mut self, args: &Value) -> Result<Value, Error> {
        let id = args.get("id").and_then(Value::as_str).ok_or_else(|| invalid("command_run needs the command `id`"))?;
        let spec = self.session.registry().get(id)?;
        if !visible(spec) {
            return Err(Error::new(ErrorKind::Unsupported, format!("{id} can only be run by a person in the app")));
        }
        let params = args.get("params").cloned().unwrap_or(Value::Object(Map::new()));
        let result = self.session.dispatch(id, &params)?;
        Ok(json!({"result": result, "revision": self.session.revision()}))
    }

    fn canvas_inspect(&self, args: &Value) -> Result<Value, Error> {
        let project = self.session.document();
        let Some(canvas) = args.get("canvas") else {
            let canvases: Vec<Value> =
                project.canvases().values().map(|c| json!({"id": c.id(), "title": c.title(), "nodes": c.nodes().len(), "edges": c.edges().len()})).collect();
            return Ok(json!({"project": {"id": project.id(), "title": project.title()}, "canvases": canvases, "revision": self.session.revision()}));
        };
        let id: CanvasId = canvas.as_str().ok_or_else(|| invalid("`canvas` must be a canvas id string"))?.parse()?;
        let canvas = project.canvases().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "no canvas with that id"))?;
        let number = |key: &str, default: usize| -> Result<usize, Error> {
            match args.get(key) {
                None => Ok(default),
                Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| invalid(format!("`{key}` must be a non-negative integer"))),
            }
        };
        let offset = number("offset", 0)?;
        let limit = number("limit", 100)?.clamp(1, MAX_PAGE);
        let nodes: Vec<_> = canvas.nodes().values().skip(offset).take(limit).collect();
        let ids: std::collections::BTreeSet<NodeId> = nodes.iter().map(|n| n.id).collect();
        let edges: Vec<_> = canvas.edges().values().filter(|e| ids.contains(&e.from.node) || ids.contains(&e.to.node)).collect();
        let next = offset.saturating_add(nodes.len());
        let more = next < canvas.nodes().len();
        Ok(json!({
            "canvas": {"id": canvas.id(), "title": canvas.title(), "nodes_total": canvas.nodes().len()},
            "nodes": serde_json::to_value(&nodes).map_err(|_| Error::new(ErrorKind::Internal, "nodes can't be serialised"))?,
            "edges": serde_json::to_value(&edges).map_err(|_| Error::new(ErrorKind::Internal, "edges can't be serialised"))?,
            "next_offset": more.then_some(next),
            "revision": self.session.revision(),
        }))
    }

    fn ids_new(&mut self, args: &Value) -> Result<Value, Error> {
        let count = match args.get("count") {
            None => 1,
            Some(v) => v.as_u64().filter(|n| (1..=MAX_NEW_IDS as u64).contains(n)).ok_or_else(|| invalid(format!("`count` must be 1–{MAX_NEW_IDS}")))?,
        };
        let ids: Vec<String> = (0..count).map(|_| NodeId::from_bytes(uuid_v4((self.new_id)())).to_string()).collect();
        Ok(json!({"ids": ids}))
    }
}

/// Marks random bytes as a version-4 UUID.
fn uuid_v4(mut b: [u8; 16]) -> [u8; 16] {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    b
}

/// Why [`serve`] stopped.
#[derive(Debug)]
pub enum ServeEnd {
    /// The client closed the input.
    Closed,
    Io(std::io::Error),
}

/// Serves MCP over newline-delimited JSON until the input ends: the stdio transport. A line
/// longer than [`MAX_MESSAGE_BYTES`] is skipped with an error reply instead of being buffered.
pub fn serve(server: &mut McpServer, input: &mut dyn BufRead, output: &mut dyn Write) -> ServeEnd {
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line(input, &mut line) {
            Ok(Line::Eof) => return ServeEnd::Closed,
            Ok(Line::TooLong) => {
                let reply = rpc_error(Value::Null, INVALID_REQUEST, format!("message is larger than {MAX_MESSAGE_BYTES} bytes")).to_string();
                if let Err(e) = writeln!(output, "{reply}").and_then(|()| output.flush()) {
                    return ServeEnd::Io(e);
                }
            }
            Ok(Line::Read) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                if let Some(reply) = server.handle(text)
                    && let Err(e) = writeln!(output, "{reply}").and_then(|()| output.flush())
                {
                    return ServeEnd::Io(e);
                }
            }
            Err(e) => return ServeEnd::Io(e),
        }
    }
}

enum Line {
    Read,
    TooLong,
    Eof,
}

/// Reads one line into `buf`, keeping at most [`MAX_MESSAGE_BYTES`]; the rest of a longer line
/// is read and dropped.
fn read_line(input: &mut dyn BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = match input.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(if !any {
                Line::Eof
            } else if too_long {
                Line::TooLong
            } else {
                Line::Read
            });
        }
        any = true;
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            let room = MAX_MESSAGE_BYTES.saturating_sub(buf.len());
            match chunk.get(..take) {
                Some(part) if part.len() <= room => buf.extend_from_slice(part),
                _ => {
                    too_long = true;
                    buf.clear();
                }
            }
        }
        input.consume(if done { take.saturating_add(1) } else { take });
        if done {
            return Ok(if too_long { Line::TooLong } else { Line::Read });
        }
    }
}
