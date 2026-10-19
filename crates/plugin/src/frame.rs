//! Framing and the protocol-1 messages.
//!
//! A frame is a 4-byte big-endian payload length, then that many bytes of UTF-8 JSON, at most
//! 1 MiB. The length is checked before anything is buffered; a bad length poisons the stream
//! (there's no way to find the next frame), so the decoder refuses everything after it.
//!
//! Every message is an object with `"protocol":1` and a `type`. Fields wider than 2^53 (revisions,
//! epochs, byte counts) are canonical decimal strings. Unknown fields and unknown types are
//! errors; no message carries a path, URL, secret or engine object.
use crate::digest::PackageDigest;
use crate::error::{Code, Result, WireError, err};
use crate::id::{RegisteredId, Token, located};
use crate::json::{self, Fields, Json, Limits};
use std::collections::BTreeMap;

/// Largest frame payload.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// The protocol major every message carries.
pub const PROTOCOL: u32 = 1;
/// Most inputs or outputs in one job.
pub const MAX_ITEMS: usize = 64;
/// Most parameters in one invocation.
pub const MAX_PARAMS: usize = 256;
/// Longest progress note, error message and idempotency key.
pub const MAX_PROGRESS_NOTE: usize = 1024;
pub const MAX_ERROR_MESSAGE: usize = 4096;
pub const MAX_IDEMPOTENCY_KEY: usize = 128;
/// Progress runs from 0 to this.
pub const PROGRESS_MAX: u32 = 1000;

/// Frames `payload` (canonical JSON of a message).
pub fn encode(message: &Message) -> Result<Vec<u8>> {
    let payload = json::canonical(&message.to_json())?;
    if payload.len() > MAX_FRAME_BYTES {
        return err(Code::TooLarge, "a frame is larger than 1 MiB");
    }
    let len = u32::try_from(payload.len()).map_err(|_| WireError::new(Code::TooLarge, "a frame is larger than 1 MiB"))?;
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload.as_bytes());
    Ok(out)
}

/// Splits a byte stream into frame payloads.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    poisoned: bool,
}

impl FrameDecoder {
    /// Adds bytes read from the stream. Never buffers more than one frame's worth beyond what
    /// has been taken.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if self.poisoned {
            return err(Code::Syntax, "the stream had a bad frame; nothing after it is read");
        }
        if self.buffer.len().saturating_add(bytes.len()) > MAX_FRAME_BYTES + 4 {
            // The caller must take frames as they complete.
            self.poisoned = true;
            return err(Code::TooLarge, "more than one frame is waiting; take frames before pushing more");
        }
        self.buffer.extend_from_slice(bytes);
        self.check_length()
    }

    fn check_length(&mut self) -> Result<()> {
        if let Some(len) = self.declared()
            && (len == 0 || len > MAX_FRAME_BYTES)
        {
            self.poisoned = true;
            self.buffer.clear();
            return err(Code::TooLarge, "a frame declares an empty or over-1-MiB payload");
        }
        Ok(())
    }

    fn declared(&self) -> Option<usize> {
        let head: [u8; 4] = self.buffer.get(..4)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(head)).ok()
    }

    /// The next whole frame's payload, if one has arrived.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        if self.poisoned {
            return err(Code::Syntax, "the stream had a bad frame; nothing after it is read");
        }
        let Some(len) = self.declared() else {
            return Ok(None);
        };
        let Some(payload) = self.buffer.get(4..4 + len) else {
            return Ok(None);
        };
        let payload = payload.to_vec();
        self.buffer.drain(..4 + len);
        self.check_length()?;
        Ok(Some(payload))
    }
}

/// The stable error codes a guest reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestErrorCode {
    InvalidInput,
    UnsupportedCapability,
    ResourceLimit,
    Cancelled,
    Conflict,
    Internal,
}

impl GuestErrorCode {
    pub const ALL: [GuestErrorCode; 6] = [
        GuestErrorCode::InvalidInput,
        GuestErrorCode::UnsupportedCapability,
        GuestErrorCode::ResourceLimit,
        GuestErrorCode::Cancelled,
        GuestErrorCode::Conflict,
        GuestErrorCode::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GuestErrorCode::InvalidInput => "InvalidInput",
            GuestErrorCode::UnsupportedCapability => "UnsupportedCapability",
            GuestErrorCode::ResourceLimit => "ResourceLimit",
            GuestErrorCode::Cancelled => "Cancelled",
            GuestErrorCode::Conflict => "Conflict",
            GuestErrorCode::Internal => "Internal",
        }
    }
}

/// A job's input or output: a role, an opaque handle and the byte count the sender claims
/// (an assertion to check against the stream, never a size to allocate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub role: Token,
    pub handle: Token,
    pub bytes: u64,
}

/// The protocol-1 messages.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// Host → guest: the versions the host speaks. The guest answers `initialized`; nothing runs.
    Initialize { protocol_minor: u32, descriptor_major: u32, descriptor_minor: u32, features: Vec<String> },
    /// Guest → host: the minor it chose, the features it uses and what it declares.
    Initialized { protocol_minor: u32, features: Vec<String>, registrations: Vec<RegisteredId> },
    /// Host → guest: run one job.
    Invoke {
        request_id: Token,
        job_id: Token,
        command_id: RegisteredId,
        package_digest: PackageDigest,
        expected_revision: u64,
        grant_epoch: u64,
        /// Parameter values by ID; their meaning is the descriptor contract's (#162).
        params: BTreeMap<String, Json>,
        inputs: Vec<Stream>,
        output_bytes: u64,
        output_items: u32,
        idempotency_key: String,
    },
    /// Guest → host: the job started.
    Accept { job_id: Token },
    /// Guest → host: 0–1000, never going back; an optional short note.
    Progress { job_id: Token, value: u32, note: Option<String> },
    /// Guest → host: the job's outputs and the effect it proposes; the host decides.
    Result { job_id: Token, grant_epoch: u64, outputs: Vec<Stream>, attach_to: Token },
    /// Guest → host: the job failed.
    Error { job_id: Token, code: GuestErrorCode, field_ids: Vec<String>, message: String },
    /// Host → guest: stop the job.
    Cancel { job_id: Token },
    /// Guest → host: the job stopped.
    CancelAck { job_id: Token },
}

impl Message {
    /// The job a message concerns, if any.
    pub fn job_id(&self) -> Option<&Token> {
        match self {
            Message::Initialize { .. } | Message::Initialized { .. } => None,
            Message::Invoke { job_id, .. }
            | Message::Accept { job_id }
            | Message::Progress { job_id, .. }
            | Message::Result { job_id, .. }
            | Message::Error { job_id, .. }
            | Message::Cancel { job_id }
            | Message::CancelAck { job_id } => Some(job_id),
        }
    }

    /// Decodes one frame's payload.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut f = Fields::new(json::parse(payload, Limits::WIRE)?, "")?;
        if f.u32("protocol")? != PROTOCOL {
            return Err(WireError::new(Code::BadVersion, "this host speaks protocol 1").at("/protocol"));
        }
        let kind = f.string("type")?;
        let message = match kind.as_str() {
            "initialize" => Message::Initialize {
                protocol_minor: f.u32("protocol_minor")?,
                descriptor_major: f.u32("descriptor_major")?,
                descriptor_minor: f.u32("descriptor_minor")?,
                features: features(&mut f)?,
            },
            "initialized" => {
                let protocol_minor = f.u32("protocol_minor")?;
                let features = features(&mut f)?;
                let items = f.array("registrations", crate::manifest::MAX_REGISTRATIONS)?;
                let registrations = items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let at = format!("/registrations/{i}");
                        located(RegisteredId::parse(&json::string(v, &at)?), &at)
                    })
                    .collect::<Result<Vec<_>>>()?;
                Message::Initialized { protocol_minor, features, registrations }
            }
            "invoke" => {
                let request_id = token(&mut f, "request_id")?;
                let job_id = token(&mut f, "job_id")?;
                let command_id = located(RegisteredId::parse(&f.string("command_id")?), "/command_id")?;
                let package_digest = located(PackageDigest::parse(&f.string("package_digest")?), "/package_digest")?;
                let expected_revision = decimal(&mut f, "expected_revision")?;
                let grant_epoch = decimal(&mut f, "grant_epoch")?;
                let params = match f.required("params")? {
                    Json::Object(map) if map.len() <= MAX_PARAMS => map,
                    Json::Object(_) => return Err(WireError::new(Code::TooLarge, "more parameters than the limit").at("/params")),
                    _ => return Err(WireError::new(Code::WrongType, "expected an object").at("/params")),
                };
                for key in params.keys() {
                    located(RegisteredId::parse(key), &format!("/params/{key}"))?;
                }
                let inputs = streams(&mut f, "inputs")?;
                let mut budget = Fields::new(f.required("output_budget")?, "/output_budget")?;
                let output_bytes = decimal(&mut budget, "bytes")?;
                let output_items = budget.u32("items")?;
                budget.finish()?;
                if output_items as usize > MAX_ITEMS {
                    return Err(WireError::new(Code::TooLarge, "more output items than the limit").at("/output_budget/items"));
                }
                let idempotency_key = f.string("idempotency_key")?;
                if idempotency_key.is_empty() || idempotency_key.len() > MAX_IDEMPOTENCY_KEY || !idempotency_key.bytes().all(|b| b.is_ascii_graphic()) {
                    return Err(WireError::new(Code::BadId, "an idempotency key is 1–128 printable ASCII bytes").at("/idempotency_key"));
                }
                Message::Invoke {
                    request_id,
                    job_id,
                    command_id,
                    package_digest,
                    expected_revision,
                    grant_epoch,
                    params,
                    inputs,
                    output_bytes,
                    output_items,
                    idempotency_key,
                }
            }
            "accept" => Message::Accept { job_id: token(&mut f, "job_id")? },
            "progress" => {
                let job_id = token(&mut f, "job_id")?;
                let value = f.u32("value")?;
                if value > PROGRESS_MAX {
                    return Err(WireError::new(Code::BadNumber, "progress runs from 0 to 1000").at("/value"));
                }
                let note = match f.optional("note") {
                    Some(v) => Some(text(json::string(v, "/note")?, MAX_PROGRESS_NOTE, "/note")?),
                    None => None,
                };
                Message::Progress { job_id, value, note }
            }
            "result" => {
                let job_id = token(&mut f, "job_id")?;
                let grant_epoch = decimal(&mut f, "grant_epoch")?;
                let outputs = streams(&mut f, "outputs")?;
                let mut effect = Fields::new(f.required("proposed_effect")?, "/proposed_effect")?;
                if effect.string("kind")? != "attach_output" {
                    return Err(WireError::new(Code::UnsupportedFeature, "the only effect a guest proposes is attach_output").at("/proposed_effect/kind"));
                }
                let attach_to = located(Token::parse(&effect.string("target_node")?), "/proposed_effect/target_node")?;
                effect.finish()?;
                Message::Result { job_id, grant_epoch, outputs, attach_to }
            }
            "error" => {
                let job_id = token(&mut f, "job_id")?;
                let code_text = f.string("code")?;
                let code = GuestErrorCode::ALL
                    .into_iter()
                    .find(|c| c.as_str() == code_text)
                    .ok_or_else(|| WireError::new(Code::WrongType, "an unknown guest error code").at("/code"))?;
                let field_ids = match f.optional("field_ids") {
                    Some(v) => json::array(v, MAX_PARAMS, "/field_ids")?
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let at = format!("/field_ids/{i}");
                            let id = json::string(v, &at)?;
                            located(RegisteredId::parse(&id), &at).map(|_| id)
                        })
                        .collect::<Result<Vec<_>>>()?,
                    None => Vec::new(),
                };
                let message = text(f.string("message")?, MAX_ERROR_MESSAGE, "/message")?;
                Message::Error { job_id, code, field_ids, message }
            }
            "cancel" => Message::Cancel { job_id: token(&mut f, "job_id")? },
            "cancel_ack" => Message::CancelAck { job_id: token(&mut f, "job_id")? },
            _ => return Err(WireError::new(Code::UnsupportedFeature, "a message type protocol 1 doesn't define").at("/type")),
        };
        f.finish()?;
        Ok(message)
    }

    /// The message as JSON (its canonical form is the frame payload).
    pub fn to_json(&self) -> Json {
        let mut m = BTreeMap::new();
        let mut put = |k: &str, v: Json| {
            m.insert(k.to_owned(), v);
        };
        let s = |t: &str| Json::Str(t.to_owned());
        let n = |v: u32| Json::Int(i64::from(v));
        let d = |v: u64| Json::Str(v.to_string());
        let list = |items: &[String]| Json::Array(items.iter().map(|i| Json::Str(i.clone())).collect());
        let streams = |items: &[Stream]| {
            Json::Array(
                items
                    .iter()
                    .map(|i| {
                        Json::Object(BTreeMap::from([
                            ("bytes".to_owned(), d(i.bytes)),
                            ("handle".to_owned(), s(i.handle.as_str())),
                            ("role".to_owned(), s(i.role.as_str())),
                        ]))
                    })
                    .collect(),
            )
        };
        put("protocol", n(PROTOCOL));
        match self {
            Message::Initialize { protocol_minor, descriptor_major, descriptor_minor, features } => {
                put("type", s("initialize"));
                put("protocol_minor", n(*protocol_minor));
                put("descriptor_major", n(*descriptor_major));
                put("descriptor_minor", n(*descriptor_minor));
                put("features", list(features));
            }
            Message::Initialized { protocol_minor, features, registrations } => {
                put("type", s("initialized"));
                put("protocol_minor", n(*protocol_minor));
                put("features", list(features));
                put("registrations", Json::Array(registrations.iter().map(|r| s(r.as_str())).collect()));
            }
            Message::Invoke {
                request_id,
                job_id,
                command_id,
                package_digest,
                expected_revision,
                grant_epoch,
                params,
                inputs,
                output_bytes,
                output_items,
                idempotency_key,
            } => {
                put("type", s("invoke"));
                put("request_id", s(request_id.as_str()));
                put("job_id", s(job_id.as_str()));
                put("command_id", s(command_id.as_str()));
                put("package_digest", s(&package_digest.to_string()));
                put("expected_revision", d(*expected_revision));
                put("grant_epoch", d(*grant_epoch));
                put("params", Json::Object(params.clone()));
                put("inputs", streams(inputs));
                put("output_budget", Json::Object(BTreeMap::from([("bytes".to_owned(), d(*output_bytes)), ("items".to_owned(), n(*output_items))])));
                put("idempotency_key", s(idempotency_key));
            }
            Message::Accept { job_id } => {
                put("type", s("accept"));
                put("job_id", s(job_id.as_str()));
            }
            Message::Progress { job_id, value, note } => {
                put("type", s("progress"));
                put("job_id", s(job_id.as_str()));
                put("value", n(*value));
                if let Some(note) = note {
                    put("note", s(note));
                }
            }
            Message::Result { job_id, grant_epoch, outputs, attach_to } => {
                put("type", s("result"));
                put("job_id", s(job_id.as_str()));
                put("grant_epoch", d(*grant_epoch));
                put("outputs", streams(outputs));
                put(
                    "proposed_effect",
                    Json::Object(BTreeMap::from([("kind".to_owned(), s("attach_output")), ("target_node".to_owned(), s(attach_to.as_str()))])),
                );
            }
            Message::Error { job_id, code, field_ids, message } => {
                put("type", s("error"));
                put("job_id", s(job_id.as_str()));
                put("code", s(code.as_str()));
                put("field_ids", list(field_ids));
                put("message", s(message));
            }
            Message::Cancel { job_id } => {
                put("type", s("cancel"));
                put("job_id", s(job_id.as_str()));
            }
            Message::CancelAck { job_id } => {
                put("type", s("cancel_ack"));
                put("job_id", s(job_id.as_str()));
            }
        }
        Json::Object(m)
    }
}

fn token(f: &mut Fields, name: &str) -> Result<Token> {
    let at = f.path(name);
    located(Token::parse(&f.string(name)?), &at)
}

fn decimal(f: &mut Fields, name: &str) -> Result<u64> {
    let at = f.path(name);
    located(json::decimal(&f.string(name)?), &at)
}

fn features(f: &mut Fields) -> Result<Vec<String>> {
    let items = f.array("features", crate::manifest::MAX_NAMES)?;
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.into_iter().enumerate() {
        let at = format!("/features/{i}");
        let name = json::string(item, &at)?;
        let shape = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
        if !shape || out.contains(&name) {
            return Err(WireError::new(Code::BadId, "a feature name is dotted lowercase, listed once").at(&at));
        }
        out.push(name);
    }
    Ok(out)
}

fn streams(f: &mut Fields, name: &str) -> Result<Vec<Stream>> {
    let base = f.path(name);
    let items = f.array(name, MAX_ITEMS)?;
    let mut out: Vec<Stream> = Vec::with_capacity(items.len());
    for (i, item) in items.into_iter().enumerate() {
        let at = format!("{base}/{i}");
        let mut s = Fields::new(item, &at)?;
        let stream = Stream { role: token(&mut s, "role")?, handle: token(&mut s, "handle")?, bytes: decimal(&mut s, "bytes")? };
        s.finish()?;
        if out.iter().any(|o| o.handle == stream.handle) {
            return Err(WireError::new(Code::Collision, "a handle is listed twice").at(&at));
        }
        out.push(stream);
    }
    Ok(out)
}

/// Plain text: bounded, without control characters other than newline and tab.
fn text(value: String, max: usize, at: &str) -> Result<String> {
    if value.len() > max {
        return Err(WireError::new(Code::TooLarge, "text longer than its limit").at(at));
    }
    if value.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(WireError::new(Code::BadString, "text may not hold control characters").at(at));
    }
    Ok(value)
}
