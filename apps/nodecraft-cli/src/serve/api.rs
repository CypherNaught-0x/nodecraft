//! The endpoints: authentication, request checks, and the JSON each answers with.
//!
//! - `GET /techniques`: the techniques the project publishes, with their forms.
//! - `POST /techniques/{technique}/runs`: queues a run; `202` with its id.
//! - `GET /runs/{id}`: a run's status, quote, steps and outputs.
//! - `GET /runs/{id}/outputs/{n}`: one output's file (or text).
use std::hash::{BuildHasher, RandomState};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use nodecraft_automation::control::Token;
use nodecraft_format::asset_source::StreamMetrics;
use serde_json::{Map, Value, json};

use super::Policy;
use super::http::{Body as HttpBody, Request, Response};
use super::webhook::Settings;
use super::worker::{Body, Book, Entry, Given, Hook, HookStatus, Job, Run, Runs, Status};

/// Longest `Idempotency-Key` taken.
const MAX_KEY: usize = 128;
/// Values one input takes at most.
const MAX_VALUES: usize = 64;

/// What every connection shares.
pub struct State {
    token: Token,
    entries: Vec<Entry>,
    runs: Arc<Runs>,
    jobs: SyncSender<Job>,
    policy: Policy,
    /// Where webhooks may go; `None` when the server sends none.
    webhooks: Option<Arc<Settings>>,
    new_id: Mutex<Box<dyn FnMut() -> [u8; 16] + Send>>,
    /// Keys the request fingerprints; random, so a client can't aim for a collision.
    hasher: RandomState,
}

fn error(status: u16, kind: &str, message: impl Into<String>) -> Response {
    Response::json(status, &json!({"error": {"kind": kind, "message": message.into()}}))
}

impl State {
    pub fn new(token: Token, entries: Vec<Entry>, runs: Arc<Runs>, jobs: SyncSender<Job>, policy: Policy, webhooks: Option<Arc<Settings>>) -> Self {
        Self { token, entries, runs, jobs, policy, webhooks, new_id: Mutex::new(Box::new(crate::random_bytes())), hasher: RandomState::new() }
    }

    /// Whether the request carries the server's token.
    pub fn authorized(&self, request: &Request) -> bool {
        let presented = request.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).unwrap_or_default();
        self.token.matches(presented.trim())
    }

    pub fn answer(&self, request: &Request) -> Response {
        if !self.authorized(request) {
            return super::http::refusal(&super::http::Refusal::UNAUTHORIZED);
        }
        let segments: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
        match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["techniques"]) => self.techniques(),
            ("POST", ["techniques", technique, "runs"]) => self.start(technique, request),
            ("GET", ["runs", id]) => self.run(id),
            ("GET", ["runs", id, "outputs", n]) => self.output(id, n),
            (_, ["techniques"] | ["techniques", _, "runs"] | ["runs", _] | ["runs", _, "outputs", _]) => {
                error(405, "method_not_allowed", "that path doesn't take this method")
            }
            _ => error(404, "not_found", "no such endpoint; see GET /techniques"),
        }
    }

    fn techniques(&self) -> Response {
        let listed: Vec<&Value> = self.entries.iter().map(|e| &e.listed).collect();
        Response::json(200, &json!({"techniques": listed}))
    }

    /// The version a request names, or the technique's latest.
    fn entry(&self, technique: &str, version: Option<u64>) -> Option<&Entry> {
        let versions = self.entries.iter().filter(|e| e.technique == technique);
        match version {
            Some(wanted) => versions.into_iter().find(|e| e.version == wanted),
            None => versions.max_by_key(|e| e.version),
        }
    }

    fn start(&self, technique: &str, request: &Request) -> Response {
        let key = match request.header("idempotency-key") {
            Some(key) if key.is_empty() || key.len() > MAX_KEY || !key.bytes().all(|b| b.is_ascii_graphic()) => {
                return error(400, "invalid_request", format!("the Idempotency-Key must be 1 to {MAX_KEY} printable ASCII characters"));
            }
            key => key.map(str::to_owned),
        };
        let fingerprint = self.hasher.hash_one((technique, &request.body));
        // A quick answer for a retry; checked again below, where it counts.
        if let Some(replayed) = key.as_deref().and_then(|key| self.replay(&self.runs.lock(), key, fingerprint)) {
            return replayed;
        }
        if !request.header("content-type").is_some_and(|t| t.split(';').next().is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))) {
            return error(415, "invalid_request", "send the run as `Content-Type: application/json`");
        }
        let body: Value = match serde_json::from_slice(&request.body) {
            Ok(Value::Object(body)) => Value::Object(body),
            _ => return error(400, "invalid_request", "the body must be a JSON object"),
        };
        let (entry, job) = match self.job(technique, &body) {
            Ok(found) => found,
            Err(response) => return response,
        };
        let id: String = {
            let mut new_id = self.new_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            new_id().iter().map(|b| format!("{b:02x}")).collect()
        };
        // Looking for the key, recording the run and queueing it happen under one lock, so two
        // requests with the same key can't both start a run, and a run is never seen without its
        // job. The worker waits for the lock only to record progress.
        let mut book = self.runs.lock();
        if let Some(replayed) = key.as_deref().and_then(|key| self.replay(&book, key, fingerprint)) {
            return replayed;
        }
        let mut run = Run::new(entry.technique.clone(), entry.version, key.map(|k| (k, fingerprint)));
        if job.webhook.is_some() {
            run.webhook = Some(Hook { status: HookStatus::Pending, attempts: 0, error: None });
        }
        if book.add(id.clone(), run).is_none() {
            return error(429, "busy", "too many runs are unfinished; try again later").header("Retry-After", "5".into());
        }
        let job = Job { id: id.clone(), ..job };
        match self.jobs.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                book.drop_run(&id);
                return error(429, "busy", "the run queue is full; try again later").header("Retry-After", "5".into());
            }
            Err(TrySendError::Disconnected(_)) => {
                book.drop_run(&id);
                return error(503, "unavailable", "the run thread has stopped; restart the server");
            }
        }
        let described = book.runs.get(&id).map_or_else(|| json!({"id": id}), |run| describe(&id, run));
        Response::json(202, &described).header("Location", format!("/runs/{id}"))
    }

    /// The run already made with `key`: itself if the request is the same, refused if not.
    fn replay(&self, book: &Book, key: &str, fingerprint: u64) -> Option<Response> {
        let (id, run) = book.keyed(key)?;
        if run.key.as_ref().is_some_and(|(_, seen)| *seen != fingerprint) {
            return Some(error(422, "idempotency_key_reuse", "this Idempotency-Key was used for a different request"));
        }
        Some(Response::json(200, &describe(id, run)).header("Location", format!("/runs/{id}")))
    }

    /// The job a run request asks for, checked against the technique's form.
    fn job(&self, technique: &str, body: &Value) -> Result<(&Entry, Job), Response> {
        let invalid = |message: String| error(422, "invalid_request", message);
        if let Some(field) =
            body.as_object().into_iter().flatten().map(|(k, _)| k.as_str()).find(|k| !["version", "inputs", "params", "max_cost", "webhook"].contains(k))
        {
            return Err(invalid(format!("unknown field `{}`", clip(field))));
        }
        let version = match &body["version"] {
            Value::Null => None,
            other => Some(other.as_u64().ok_or_else(|| invalid("`version` must be a version number".into()))?),
        };
        let entry =
            self.entry(technique, version).ok_or_else(|| error(404, "not_found", "the project publishes no such technique version; see GET /techniques"))?;
        let mut inputs = Vec::new();
        for (port, given) in object(&body["inputs"], "inputs").map_err(invalid)? {
            let spec = entry.inputs.iter().find(|i| i.port == *port).ok_or_else(|| invalid(format!("the technique has no input `{}`", clip(port))))?;
            if !spec.fillable {
                return Err(invalid(format!("the input `{}` can't be filled by a request", clip(port))));
            }
            let values = match given {
                Value::Array(values) if values.len() > MAX_VALUES => return Err(invalid(format!("`{}` takes at most {MAX_VALUES} values", clip(port)))),
                Value::Array(values) => values.iter().map(|v| self::given(port, v)).collect::<Result<Vec<_>, _>>(),
                one => self::given(port, one).map(|v| vec![v]),
            }
            .map_err(invalid)?;
            inputs.push((port.clone(), values));
        }
        let mut params = Vec::new();
        for (name, value) in object(&body["params"], "params").map_err(invalid)? {
            if !entry.params.contains(name) {
                return Err(invalid(format!("the technique has no parameter `{}`", clip(name))));
            }
            params.push((name.clone(), typed(value).ok_or_else(|| invalid(format!("give `{}` as text, true/false or a typed value", clip(name))))?));
        }
        let mut policy = self.policy.clone();
        for (code, amount) in object(&body["max_cost"], "max_cost").map_err(invalid)? {
            let asked = amount.as_str().and_then(super::micros).ok_or_else(|| invalid(format!("`max_cost.{}` must be an amount like \"1.50\"", clip(code))))?;
            // A request can only lower the server's limit.
            if let Some(limit) = policy.max_cost.get_mut(code.as_str()) {
                *limit = (*limit).min(asked);
            }
        }
        let webhook = match (&body["webhook"], &self.webhooks) {
            (Value::Null, _) => None,
            (_, None) => return Err(invalid("this server sends no webhooks; start it with --webhook-allow, or poll the run".into())),
            (Value::String(url), Some(webhooks)) => Some(webhooks.allows(url).map_err(invalid)?),
            (_, Some(_)) => return Err(invalid("`webhook` must be a URL".into())),
        };
        Ok((entry, Job { id: String::new(), canvas: entry.canvas, inputs, params, names: entry.outputs.clone(), policy, webhook }))
    }

    fn run(&self, id: &str) -> Response {
        let book = self.runs.lock();
        match book.runs.get(id) {
            Some(run) => Response::json(200, &describe(id, run)),
            None => error(404, "not_found", "no such run; runs are forgotten when the server restarts, or the oldest when it holds too many"),
        }
    }
}

/// A run as `GET /runs/{id}` and its webhook show it.
pub fn describe(id: &str, run: &Run) -> Value {
    let outputs: Vec<Value> = run
        .outputs
        .iter()
        .enumerate()
        .map(|(n, o)| {
            let mut shown = json!({"n": n, "name": o.name, "port": o.port});
            if let Some(item) = o.item {
                shown["item"] = json!(item);
            }
            match &o.body {
                Body::Text(text) => {
                    shown["type"] = json!("text");
                    shown["text"] = json!(text);
                }
                Body::Media { mime, asset } => {
                    shown["type"] = json!("media");
                    shown["mime"] = json!(mime);
                    shown["bytes"] = json!(asset.byte_len());
                    shown["url"] = json!(format!("/runs/{id}/outputs/{n}"));
                }
                Body::Value(value) => {
                    shown["type"] = json!("value");
                    shown["value"] = value.clone();
                }
                Body::Expired { mime } => {
                    shown["type"] = json!("media");
                    shown["mime"] = json!(mime);
                    shown["expired"] = json!(true);
                }
            }
            shown
        })
        .collect();
    let mut described =
        json!({"id": id, "status": run.status.name(), "technique": run.technique, "version": run.version, "steps": run.steps, "outputs": outputs});
    if let Some(quote) = &run.quote {
        described["cost"] = json!({"totals": quote["totals"], "complete": quote["complete"]});
    }
    if run.expired {
        described["expired"] = json!(true);
    }
    if let Some(error) = &run.error {
        described["error"] = json!({"kind": error.kind, "message": error.message});
    }
    if let Some(hook) = &run.webhook {
        described["webhook"] = json!({"status": hook.status.name(), "attempts": hook.attempts});
        if let Some(error) = &hook.error {
            described["webhook"]["error"] = json!(error);
        }
    }
    described
}

impl State {
    fn output(&self, id: &str, n: &str) -> Response {
        let book = self.runs.lock();
        let Some(run) = book.runs.get(id) else { return error(404, "not_found", "no such run") };
        if run.status != Status::Succeeded {
            return error(409, "not_ready", format!("the run is {}", run.status.name()));
        }
        let output = n.parse::<usize>().ok().and_then(|n| run.outputs.get(n));
        match output.map(|o| &o.body) {
            Some(Body::Media { mime, asset }) => Response {
                status: 200,
                content_type: "application/octet-stream",
                headers: vec![("X-Content-Mime", mime.clone())],
                body: HttpBody::Stream(Box::new(asset.reader(StreamMetrics::default())), asset.byte_len()),
            },
            Some(Body::Text(text)) => {
                Response { status: 200, content_type: "text/plain; charset=utf-8", headers: Vec::new(), body: HttpBody::Bytes(text.clone().into_bytes()) }
            }
            Some(Body::Value(value)) => Response::json(200, value),
            Some(Body::Expired { .. }) => error(410, "expired", "this run's files were let go to make room for newer runs"),
            None => error(404, "not_found", "the run has no such output"),
        }
    }
}

/// A field that must be an object (or absent).
fn object<'a>(value: &'a Value, field: &str) -> Result<&'a Map<String, Value>, String> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    match value {
        Value::Null => Ok(EMPTY.get_or_init(Map::new)),
        Value::Object(map) => Ok(map),
        _ => Err(format!("`{field}` must be an object")),
    }
}

/// A request's name, shortened for an error message.
fn clip(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// Plain text or a flag as the engine's typed value; a typed value as given.
fn typed(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => Some(json!({"type": "text", "value": text})),
        Value::Bool(flag) => Some(json!({"type": "bool", "value": flag})),
        Value::Object(map) if map.contains_key("type") => Some(value.clone()),
        _ => None,
    }
}

/// One input value: `{"file": base64, "name": ...}` for media, otherwise as [`typed`].
fn given(port: &str, value: &Value) -> Result<Given, String> {
    if let Some(file) = value.get("file") {
        let name = value.get("name").and_then(Value::as_str).unwrap_or("upload");
        let bytes = file.as_str().and_then(base64).ok_or_else(|| format!("`{}`'s file must be base64", clip(port)))?;
        return Ok(Given::Media { name: clip(name), bytes });
    }
    typed(value).map(Given::Value).ok_or_else(|| format!("give `{}` as text, a typed value or {{\"file\": base64}}", clip(port)))
}

/// Standard base64, padded correctly or not at all; whitespace and stray bits are refused.
pub fn base64(text: &str) -> Option<Vec<u8>> {
    let data = text.trim_end_matches('=');
    let padding = text.len().saturating_sub(data.len());
    if padding > 2 || (padding > 0 && !text.len().is_multiple_of(4)) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 3);
    let mut bits: u32 = 0;
    let mut count = 0;
    for byte in data.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = ((bits << 6) | u32::from(value)) & 0xffff;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(((bits >> count) & 0xff) as u8);
        }
    }
    // Six leftover bits can't come from whole bytes, and the ones left over must be zero.
    if count >= 6 || bits & ((1 << count) - 1) != 0 {
        return None;
    }
    Some(out)
}
