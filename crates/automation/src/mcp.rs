//! The MCP server: NodeCraft's engine commands as Model Context Protocol tools.
//!
//! [`McpServer`] is sans-IO: [`McpServer::handle`] takes one JSON-RPC message and returns the
//! reply, if any. [`serve`] runs it over newline-delimited JSON (the MCP stdio transport).
//!
//! The tools are generic, so they track the command registry without a second implementation:
//! - `command_list`: the commands an agent may run, with their parameter schemas;
//! - `command_run`: runs one command through the same `Session::dispatch` as the UI;
//! - `canvas_inspect`: the project's canvases, one canvas's nodes and edges (paged, size-capped),
//!   or one node in full;
//! - `ids_new`: fresh ids for commands that take caller-supplied ids (`node.add`, `node.create`, edge commands).
//!
//! Every command is classified: [`AGENT_COMMANDS`] may be listed and run, [`HUMAN_ONLY`] never.
//! An unclassified command stays hidden, and a test fails until it is classified.

mod overview;

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use nodecraft_engine::Session;
use nodecraft_graph::Edge;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::{Map, Value, json};

/// Protocol revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest message accepted, in bytes. The engine takes at most 1 MiB of decoded command
/// parameters; clients may escape non-ASCII text (`\uXXXX`, up to 6 bytes a character), so this
/// leaves room for that while bounding the memory a parse can take.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
/// Exact encoded JSON cap for a project overview, including its envelope. Node/edge pages
/// retain their rough payload budget. Tool results duplicate JSON as escaped text: this is
/// not a final wire-size cap, and an overview reply can exceed 1 MiB on the wire.
pub const PAGE_BUDGET_BYTES: usize = 512 * 1024;
/// Nodes larger than this are summarised in pages; fetch them one at a time with `node`.
pub const NODE_INLINE_BYTES: usize = 64 * 1024;
/// Most nodes `canvas_inspect` returns per page.
pub const MAX_PAGE: usize = 500;
/// Most ids `ids_new` hands out per call.
pub const MAX_NEW_IDS: usize = 100;

/// Commands an agent may see and run.
pub const AGENT_COMMANDS: &[&str] = &[
    "node.add",
    "node.create",
    "node.remove",
    "node.move",
    "node.title",
    "node.param_set",
    "edge.connect",
    "edge.disconnect",
    "edge.rewire",
    "canvas.delete",
    "canvas.paste",
    "edit.undo",
    "edit.redo",
    "canvas.stale",
    "canvas.fit_all",
    "canvas.fit_selection",
    "canvas.minimap_toggle",
    "canvas.pan_to",
    "canvas.search_open",
    "canvas.search_query",
    "canvas.search_next",
    "canvas.search_previous",
    "canvas.search_close",
    "comment.create",
    "comment.edit",
    "comment.reply",
    "comment.reply_remove",
    "comment.move",
    "comment.resolve",
    "comment.remove",
    "comment.open",
    "comment.compose",
    "canvas.comments_toggle",
    "keys.status",
];
/// Commands only a person may run: `keys.set` takes a secret, which must not pass through an
/// agent's context, and `keys.remove` deletes a key with no undo. File commands remain human-only
/// until MCP/control enforce allowed read/write roots (#116); native path canonicalization is not
/// filesystem authorization. Engine/native callers retain their existing file access.
pub const HUMAN_ONLY: &[&str] = &["keys.set", "keys.remove", "file.open", "file.save", "file.save_as"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Where fresh ids come from (randomness belongs to the host platform).
pub type IdSource = Box<dyn FnMut() -> [u8; 16]>;

/// One MCP connection: over its own engine session, or bridged to a running app.
pub struct McpServer {
    backend: Backend,
    new_id: IdSource,
}

enum Backend {
    /// A headless session of its own.
    Local(Box<Session>),
    /// The running app's session, through its control channel (`nodecraft --control`).
    #[cfg(not(target_arch = "wasm32"))]
    Bridge(crate::control::Client),
}

/// Whether automation may see and run the command `id`.
pub fn visible(id: &str) -> bool {
    AGENT_COMMANDS.contains(&id)
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

fn internal() -> Error {
    Error::new(ErrorKind::Internal, "the canvas can't be serialised")
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn tools() -> Value {
    json!([
        {
            "name": "command_list",
            "title": "List commands",
            "description": "Lists the NodeCraft commands you can run with command_run: id, label and parameter schema. Commands without parameters also say whether they can run right now (for example edit.undo when there is nothing to undo). Filter with `query` (matches id or label).",
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
            "description": "Without `canvas`: the project and a page of canvas summaries sorted by id, using `offset` and `limit`; includes `total`, `next_offset` and `revision`. Compare revisions and restart paging after edits. Overview structured JSON is at most 512 KiB; escaped text duplication adds wire bytes. With `canvas`: a page of that canvas's nodes (sorted by id, from `offset`, at most `limit`, and cut short to keep the reply small; continue from `next_offset`) with the edges into them. Nodes too large for a page are summarised; read one in full with `canvas` and `node`.",
            "inputSchema": object_schema(json!({
                "canvas": {"type": "string", "description": "Canvas id (UUID)"},
                "node": {"type": "string", "description": "Node id (UUID): return just this node, in full, with its edges paged by `edge_offset`"},
                "edge_offset": {"type": "integer", "minimum": 0},
                "offset": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE}
            }), &[]),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "ids_new",
            "title": "New ids",
            "description": "Fresh unique ids (UUIDs) for commands that take a caller-chosen id, such as node.add, node.create or edge.connect.",
            "inputSchema": object_schema(json!({"count": {"type": "integer", "minimum": 1, "maximum": MAX_NEW_IDS}}), &[]),
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }
    ])
}

/// Bytes of `value` as JSON, counted without building the text.
fn json_len(value: &impl serde::Serialize) -> Result<usize, Error> {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_err(|_| internal())?;
    Ok(count.0)
}

/// As many of `edges`, from `offset`, as fit in `budget` bytes (at least one), and where to
/// continue.
fn edge_page<'a>(edges: &[&'a Edge], offset: usize, budget: usize) -> Result<(Vec<&'a Edge>, Option<usize>), Error> {
    let mut page = Vec::new();
    let mut used = 0usize;
    for e in edges.iter().skip(offset) {
        let n = json_len(e)?;
        if !page.is_empty() && used.saturating_add(n) > budget {
            break;
        }
        used = used.saturating_add(n);
        page.push(*e);
    }
    let next = offset.saturating_add(page.len());
    Ok((page, (next < edges.len()).then_some(next)))
}

pub(crate) fn command_list(session: &Session, args: &Map<String, Value>) -> Result<Value, Error> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or_default().to_lowercase();
    let commands: Vec<Value> = session
        .registry()
        .specs()
        .filter(|s| visible(s.id))
        .filter(|s| query.is_empty() || s.id.contains(&query) || s.label.to_lowercase().contains(&query))
        .map(|s| {
            let schema = (s.input_schema)();
            let takes_params = schema.get("required").and_then(Value::as_array).is_some_and(|r| !r.is_empty());
            let mut entry = json!({"id": s.id, "label": s.label, "params_schema": schema, "params_doc": s.params_doc});
            // Whether a command can run depends on its params; only parameterless ones are
            // checked here, so `ready` always means the same thing.
            if !takes_params {
                let enabled = session.enabled(s.id, &json!({}));
                entry["ready"] = json!(enabled.is_ok());
                if let Err(e) = enabled {
                    entry["not_ready_because"] = json!(e.message);
                }
            }
            entry
        })
        .collect();
    Ok(json!({"commands": commands}))
}

pub(crate) fn command_run(session: &mut Session, mut args: Map<String, Value>) -> Result<Value, Error> {
    let id = match args.remove("id") {
        Some(Value::String(id)) => id,
        _ => return Err(invalid("command_run needs the command `id`")),
    };
    session.registry().get(&id)?;
    if !visible(&id) {
        let why = if HUMAN_ONLY.contains(&id.as_str()) { "can only be run by a person in the app" } else { "isn't available to automation yet" };
        return Err(Error::new(ErrorKind::Unsupported, format!("{id} {why}")));
    }
    let params = args.remove("params").unwrap_or(Value::Object(Map::new()));
    let result = session.dispatch(&id, &params)?;
    Ok(json!({"result": result, "revision": session.revision()}))
}

pub(crate) fn canvas_inspect(session: &Session, args: &Map<String, Value>) -> Result<Value, Error> {
    let project = session.document();
    let number = |key: &str, default: usize| -> Result<usize, Error> {
        match args.get(key) {
            None => Ok(default),
            Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| invalid(format!("`{key}` must be a non-negative integer"))),
        }
    };
    let Some(canvas) = args.get("canvas") else {
        if args.contains_key("node") {
            return Err(invalid("`node` needs its `canvas`"));
        }
        if args.contains_key("edge_offset") {
            return Err(invalid("`edge_offset` needs `canvas` and `node`"));
        }
        let offset = number("offset", 0)?;
        let limit = number("limit", 100)?;
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(invalid(format!("`limit` must be 1–{MAX_PAGE}")));
        }
        return overview::page(project, offset, limit, session.revision(), PAGE_BUDGET_BYTES);
    };
    let id: CanvasId = canvas.as_str().ok_or_else(|| invalid("`canvas` must be a canvas id string"))?.parse()?;
    let canvas = project.canvases().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "no canvas with that id"))?;
    let revision = session.revision();
    if let Some(node) = args.get("node") {
        let node: NodeId = node.as_str().ok_or_else(|| invalid("`node` must be a node id string"))?.parse()?;
        let node = canvas.nodes().get(&node).ok_or_else(|| Error::new(ErrorKind::NotFound, "no node with that id on this canvas"))?;
        // The node in full (the graph caps it at 8 MiB); its edges in pages, by edge id.
        let touching: Vec<&Edge> = canvas.edges().values().filter(|e| e.to.node == node.id || e.from.node == node.id).collect();
        let (edges, next) = edge_page(&touching, number("edge_offset", 0)?, PAGE_BUDGET_BYTES)?;
        return Ok(json!({"node": node, "edges": edges, "edges_total": touching.len(), "next_edge_offset": next, "revision": revision}));
    }
    let offset = number("offset", 0)?;
    let limit = number("limit", 100)?.clamp(1, MAX_PAGE);
    // Each edge is listed with the node it enters, so paging lists every edge once.
    let mut incoming: BTreeMap<NodeId, Vec<&Edge>> = BTreeMap::new();
    for e in canvas.edges().values() {
        incoming.entry(e.to.node).or_default().push(e);
    }
    let mut nodes = Vec::new();
    let mut edges: Vec<&Edge> = Vec::new();
    let mut used = 0usize;
    let mut edges_truncated = false;
    let mut taken = 0usize;
    let mut truncated_at: Option<NodeId> = None;
    for node in canvas.nodes().values().skip(offset).take(limit) {
        let entry = if json_len(node)? > NODE_INLINE_BYTES {
            json!({"id": node.id, "kind": node.kind, "title": node.title, "pos": node.pos, "params": node.params.keys().collect::<Vec<_>>(), "history": node.history.len(), "summarised": "too large for a page; read it with `node`"})
        } else {
            serde_json::to_value(node).map_err(|_| internal())?
        };
        let into = incoming.get(&node.id).map(Vec::as_slice).unwrap_or_default();
        let cost = json_len(&entry)?.saturating_add(json_len(&into)?);
        if taken > 0 && used.saturating_add(cost) > PAGE_BUDGET_BYTES {
            break;
        }
        used = used.saturating_add(json_len(&entry)?);
        nodes.push(entry);
        // A first node with more incoming edges than a page holds gets as many as fit; the
        // rest are read with `node`.
        for e in into {
            let n = json_len(e)?;
            if used.saturating_add(n) > PAGE_BUDGET_BYTES {
                edges_truncated = true;
                truncated_at = Some(node.id);
                break;
            }
            used = used.saturating_add(n);
            edges.push(e);
        }
        taken = taken.saturating_add(1);
    }
    let next = offset.saturating_add(taken);
    let more = next < canvas.nodes().len();
    Ok(json!({
        "canvas": {"id": canvas.id(), "title": canvas.title(), "nodes_total": canvas.nodes().len(), "edges_total": canvas.edges().len()},
        "nodes": nodes,
        "edges": serde_json::to_value(&edges).map_err(|_| internal())?,
        "edges_truncated": edges_truncated,
        "edges_continue": truncated_at.map(|n| json!({"node": n, "hint": "read all of this node's edges with canvas_inspect `node` (paged by `edge_offset`)"})),
        "next_offset": more.then_some(next),
        "revision": revision,
    }))
}

impl McpServer {
    pub fn new(session: Session, new_id: IdSource) -> Self {
        Self { backend: Backend::Local(Box::new(session)), new_id }
    }

    /// A server that works on the running app (`nodecraft --control`) through `client`: the
    /// agent edits the canvas the user sees, and can take screenshots of it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn bridge(client: crate::control::Client, new_id: IdSource) -> Self {
        Self { backend: Backend::Bridge(client), new_id }
    }

    /// The local session (`None` when bridged to the app).
    pub fn session(&self) -> Option<&Session> {
        match &self.backend {
            Backend::Local(session) => Some(session),
            #[cfg(not(target_arch = "wasm32"))]
            Backend::Bridge(_) => None,
        }
    }

    fn bridged(&self) -> bool {
        !matches!(self.backend, Backend::Local(_))
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
        let Value::Object(mut obj) = message else {
            // JSON-RPC batches were removed from MCP in 2025-06-18.
            return Some(rpc_error(Value::Null, INVALID_REQUEST, "expected a single JSON-RPC message object"));
        };
        let id = obj.remove("id");
        let is_reply = obj.contains_key("result") || obj.contains_key("error");
        let method = match obj.remove("method") {
            Some(Value::String(m)) => m,
            // A response to a request we never sent: nothing to do.
            _ if is_reply => return None,
            _ => return id.map(|id| rpc_error(id, INVALID_REQUEST, "missing method")),
        };
        // Notifications (`notifications/initialized`, `notifications/cancelled`, …) get no reply.
        let id = id?;
        if !(id.is_string() || id.is_i64() || id.is_u64()) {
            return Some(rpc_error(Value::Null, INVALID_REQUEST, "the request id must be a string or an integer"));
        }
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(rpc_error(id, INVALID_REQUEST, "expected \"jsonrpc\": \"2.0\""));
        }
        let params = obj.remove("params").unwrap_or(Value::Object(Map::new()));
        let reply = match method.as_str() {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.tools()})),
            "tools/call" => self.call(params),
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

    fn call(&mut self, params: Value) -> Result<Value, (i64, String)> {
        let Value::Object(mut params) = params else { return Err((INVALID_PARAMS, "tools/call params must be an object".into())) };
        let name = match params.remove("name") {
            Some(Value::String(n)) => n,
            _ => return Err((INVALID_PARAMS, "tools/call needs a tool name".into())),
        };
        let args = match params.remove("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(args)) => args,
            Some(_) => return Err((INVALID_PARAMS, "tool arguments must be an object".into())),
        };
        let unknown = || Err((INVALID_PARAMS, format!("unknown tool {}", name.chars().take(64).collect::<String>())));
        let result = match (&mut self.backend, name.as_str()) {
            (_, "ids_new") => self.ids_new(&args),
            (Backend::Local(session), "command_list") => command_list(session, &args),
            (Backend::Local(session), "command_run") => command_run(session, args),
            (Backend::Local(session), "canvas_inspect") => canvas_inspect(session, &args),
            #[cfg(not(target_arch = "wasm32"))]
            (Backend::Bridge(client), tool @ ("command_list" | "command_run" | "canvas_inspect")) => {
                let method = match tool {
                    "command_list" => "commands.list",
                    "command_run" => "command.run",
                    _ => "canvas.inspect",
                };
                client.call(method, args)
            }
            #[cfg(not(target_arch = "wasm32"))]
            (Backend::Bridge(client), "screenshot") => {
                let image = client.call("screenshot", Map::new()).and_then(|mut shot| {
                    let shot = shot.as_object_mut().ok_or_else(internal)?;
                    // Moved, not cloned: a screenshot can be tens of megabytes.
                    match (shot.remove("data"), shot.remove("mime")) {
                        (Some(Value::String(data)), Some(Value::String(mime))) => Ok((data, mime)),
                        _ => Err(Error::new(ErrorKind::Internal, "the app sent a screenshot without image data")),
                    }
                });
                return Ok(match image {
                    Ok((data, mime)) => json!({"content": [{"type": "image", "data": data, "mimeType": mime}], "isError": false}),
                    Err(e) => tool_err(&e),
                });
            }
            _ => return unknown(),
        };
        Ok(match result {
            Ok(v) => tool_ok(v),
            Err(e) => tool_err(&e),
        })
    }

    fn tools(&self) -> Value {
        let mut list = tools();
        if self.bridged()
            && let Some(items) = list.as_array_mut()
        {
            items.push(json!({
                "name": "screenshot",
                "title": "Screenshot the app",
                "description": "A PNG of the NodeCraft window as the user sees it right now.",
                "inputSchema": object_schema(json!({}), &[]),
                "annotations": {"readOnlyHint": true, "openWorldHint": false}
            }));
        }
        list
    }

    fn ids_new(&mut self, args: &Map<String, Value>) -> Result<Value, Error> {
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

/// Serves MCP over newline-delimited JSON until the input ends: the stdio transport. Messages are
/// handled one at a time, in order. A line longer than [`MAX_MESSAGE_BYTES`] is skipped with an
/// error reply instead of being buffered.
pub fn serve(server: &mut McpServer, input: &mut dyn BufRead, output: &mut dyn Write) -> ServeEnd {
    let mut line = Vec::new();
    loop {
        line.clear();
        let reply = match read_line(input, &mut line) {
            Ok(Line::Eof) => return ServeEnd::Closed,
            Ok(Line::TooLong) => Some(rpc_error(Value::Null, INVALID_REQUEST, format!("message is larger than {MAX_MESSAGE_BYTES} bytes")).to_string()),
            Ok(Line::Read) => match std::str::from_utf8(&line) {
                Ok(text) if text.trim().is_empty() => None,
                Ok(text) => server.handle(text.trim()),
                Err(_) => Some(rpc_error(Value::Null, PARSE_ERROR, "the message isn't valid UTF-8").to_string()),
            },
            Err(e) => return ServeEnd::Io(e),
        };
        if let Some(reply) = reply
            && let Err(e) = writeln!(output, "{reply}").and_then(|()| output.flush())
        {
            return ServeEnd::Io(e);
        }
    }
}

pub(crate) enum Line {
    Read,
    TooLong,
    Eof,
}

/// Reads one line into `buf`, keeping at most [`MAX_MESSAGE_BYTES`]; the rest of a longer line
/// is read and dropped.
pub(crate) fn read_line(input: &mut dyn BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    read_line_capped(input, buf, MAX_MESSAGE_BYTES)
}

/// [`read_line`] with a different cap (the bridge reads screenshot replies).
pub(crate) fn read_line_capped(input: &mut dyn BufRead, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<Line> {
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
            let room = cap.saturating_sub(buf.len());
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
