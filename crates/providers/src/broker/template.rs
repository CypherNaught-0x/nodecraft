//! A provider operation template (schema 1), decoded and checked against the host's ceilings.
//!
//! Destinations are fixed by the template, never by a job or a response: one API origin, plus
//! the origins outputs may be downloaded from. Each is `https://` and a public DNS name (no IP
//! literal, port, user, path, or local name). The credential goes where the host's fixed list of
//! placements says, and the template names no other header. Every model has a positive price per
//! job: a broker request carries a credential to a paid service, so it is never quoted as free.

use std::collections::BTreeSet;

use nodecraft_types::{Modality, Money};
use serde::Deserialize;
use serde_json::Value;

/// The template schema this host reads (`provider_operations` runtime schema 1).
pub const TEMPLATE_SCHEMA: u32 = 1;
/// Largest template file.
pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
pub const MAX_MODELS: usize = 64;
pub const MAX_OUTPUT_ORIGINS: usize = 4;
/// Most output fields an operation may read, and values an array field may expand to.
pub const MAX_OUTPUTS: usize = 16;
/// Most status values per state.
pub const MAX_STATES: usize = 16;
/// Deepest request body.
pub const MAX_BODY_DEPTH: usize = 12;
pub const MAX_PLACEHOLDERS: usize = 64;
/// Host ceilings: a template may ask for less, never more.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TIMEOUT_MS: u64 = 120_000;
pub const MAX_CONCURRENCY: u32 = 4;
pub const MIN_POLL_MS: u64 = 500;
pub const MAX_POLL_MS: u64 = 60_000;
const MAX_LABEL: usize = 64;
const MAX_PATH: usize = 256;
const MAX_NAME: usize = 64;
const MAX_MODEL_ID: usize = 128;
const MAX_POINTER: usize = 128;
const MAX_STATUS: usize = 64;

/// Names that only resolve on a local network, or to the machine itself.
const LOCAL_SUFFIXES: [&str; 8] = ["localhost", "local", "internal", "lan", "home", "corp", "intranet", "home.arpa"];

/// Why a template was refused: what is wrong, and where (a JSON pointer into the template).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    pub at: String,
    pub message: String,
}

impl TemplateError {
    fn new(at: impl Into<String>, message: impl Into<String>) -> Self {
        Self { at: at.into(), message: message.into() }
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.at.is_empty() { f.write_str(&self.message) } else { write!(f, "{} (at {})", self.message, self.at) }
    }
}

impl std::error::Error for TemplateError {}

type Result<T> = std::result::Result<T, TemplateError>;

/// An `https://` origin on a public DNS name, in canonical form (`https://api.example.com`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin(String);

impl Origin {
    pub fn parse(text: &str) -> std::result::Result<Self, &'static str> {
        const SHAPE: &str = "an origin is https:// and a public DNS name, with no port, path, user, query or fragment";
        let url = url::Url::parse(text).map_err(|_| SHAPE)?;
        let canonical = url.origin().ascii_serialization();
        if url.scheme() != "https" || canonical != text || url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err(SHAPE);
        }
        let Some(url::Host::Domain(host)) = url.host() else { return Err("an origin names a DNS host, not an IP address") };
        if !public_name(host) {
            return Err("an origin's host must be a public name, not a local or internal one");
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host name.
    pub fn host(&self) -> &str {
        self.0.strip_prefix("https://").unwrap_or(&self.0)
    }

    /// Whether `url` is on this origin.
    pub fn holds(&self, url: &url::Url) -> bool {
        url.origin().ascii_serialization() == self.0
    }
}

fn public_name(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    let tld_ok = labels.last().is_some_and(|tld| (tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_lowercase())) || tld.starts_with("xn--"));
    let labels_ok = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    let local = LOCAL_SUFFIXES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}")));
    labels.len() >= 2 && host.len() <= 253 && tld_ok && labels_ok && !local
}

/// Where the host puts the credential. The template picks one of these; it never names a header
/// of its own, so a key can only go where providers expect one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Credential {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `Authorization: Key <key>`
    Key,
    /// `x-api-key: <key>`
    XApiKey,
    /// `api-key: <key>`
    ApiKey,
    /// `x-goog-api-key: <key>`
    XGoogApiKey,
}

impl Credential {
    /// The header and its value for `key`.
    pub fn header(self, key: &str) -> (&'static str, String) {
        match self {
            Credential::Bearer => ("authorization", format!("Bearer {key}")),
            Credential::Key => ("authorization", format!("Key {key}")),
            Credential::XApiKey => ("x-api-key", key.to_owned()),
            Credential::ApiKey => ("api-key", key.to_owned()),
            Credential::XGoogApiKey => ("x-goog-api-key", key.to_owned()),
        }
    }
}

/// The header the host sends its idempotency key in, where the provider honours one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdempotencyHeader {
    IdempotencyKey,
    XIdempotencyKey,
}

impl IdempotencyHeader {
    pub fn name(self) -> &'static str {
        match self {
            IdempotencyHeader::IdempotencyKey => "idempotency-key",
            IdempotencyHeader::XIdempotencyKey => "x-idempotency-key",
        }
    }
}

/// A model the template serves, and the most one job of it may cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateModel {
    pub id: String,
    /// What a person picks it by; its ID when the template names none.
    pub label: String,
    /// What it makes: text when every output is text, an image when the template says nothing.
    pub output: Modality,
    /// An upper bound per job, in exact money. The host's quote never goes below it.
    pub price: Money,
}

/// Where in a response an output is, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputField {
    /// A JSON pointer to a string, or an array of strings.
    pub at: String,
    pub kind: OutputKind,
    pub mime: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// Text the model returned.
    Text,
    /// A file to download, from the template's origin or one of its output origins.
    Url,
}

/// A path under the origin, with `{model}` and `{job}` placeholders, each standing for one
/// value the host checks before substituting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTemplate(pub(crate) String);

/// How the submit request is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitReply {
    /// A job to poll, whose id is at this pointer.
    Queued { handle: String },
    /// The outputs, in the reply itself.
    Finished { outputs: Vec<OutputField> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubmitOperation {
    pub path: PathTemplate,
    /// The JSON body; strings `"{model}"`, `"{param:NAME}"` and `"{text:ROLE}"` are whole-value
    /// placeholders the host fills from the job.
    pub body: Value,
    pub reply: SubmitReply,
}

/// Status values sorted into the host's job states.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct States {
    pub queued: BTreeSet<String>,
    pub running: BTreeSet<String>,
    pub done: BTreeSet<String>,
    pub failed: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOperation {
    pub path: PathTemplate,
    pub status: String,
    pub states: States,
    pub outputs: Vec<OutputField>,
    /// A pointer to the provider's error message for a failed job.
    pub error: Option<String>,
    pub interval_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CancelMethod {
    Post,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelOperation {
    pub method: CancelMethod,
    pub path: PathTemplate,
}

/// Bounds on every request the broker sends for this template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub response_bytes: usize,
    pub timeout_ms: u64,
    pub concurrency: u32,
}

/// A checked provider operation template.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub label: String,
    pub origin: Origin,
    pub output_origins: Vec<Origin>,
    pub credential: Option<Credential>,
    /// Without one, an ambiguous submit is never retried automatically: it may have been charged.
    pub idempotency: Option<IdempotencyHeader>,
    pub models: Vec<TemplateModel>,
    pub submit: SubmitOperation,
    pub poll: Option<PollOperation>,
    pub cancel: Option<CancelOperation>,
    pub limits: Limits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplate {
    schema: u32,
    label: String,
    origin: String,
    #[serde(default)]
    output_origins: Vec<String>,
    #[serde(default)]
    credential: Option<Credential>,
    #[serde(default)]
    idempotency: Option<IdempotencyHeader>,
    models: Vec<RawModel>,
    submit: RawSubmit,
    #[serde(default)]
    poll: Option<RawPoll>,
    #[serde(default)]
    cancel: Option<RawCancel>,
    limits: RawLimits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    output: Option<Modality>,
    price: Money,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    at: String,
    kind: OutputKind,
    #[serde(default)]
    mime: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSubmit {
    method: String,
    path: String,
    body: Value,
    #[serde(default)]
    handle: Option<String>,
    #[serde(default)]
    outputs: Option<Vec<RawOutput>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStates {
    #[serde(default)]
    queued: Vec<String>,
    #[serde(default)]
    running: Vec<String>,
    done: Vec<String>,
    failed: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPoll {
    method: String,
    path: String,
    status: String,
    states: RawStates,
    outputs: Vec<RawOutput>,
    #[serde(default)]
    error: Option<String>,
    interval_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCancel {
    method: CancelMethod,
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    response_bytes: usize,
    timeout_ms: u64,
    concurrency: u32,
}

impl Template {
    /// Decodes and checks a template file.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::new("", "a provider template is at most 64 KiB"));
        }
        let raw: RawTemplate = serde_json::from_slice(bytes).map_err(|e| TemplateError::new("", format!("not a provider template: {}", short(&e))))?;
        check(raw)
    }

    /// The roles of the `{text:ROLE}` placeholders its submit body fills.
    pub fn text_roles(&self) -> BTreeSet<&str> {
        fn walk<'a>(value: &'a Value, out: &mut BTreeSet<&'a str>) {
            match value {
                Value::String(s) => {
                    if let Ok(Some(Placeholder::Text(role))) = placeholder(s) {
                        out.insert(role);
                    }
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                Value::Object(map) => map.values().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        let mut out = BTreeSet::new();
        walk(&self.submit.body, &mut out);
        out
    }

    pub fn model(&self, id: &str) -> Option<&TemplateModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Every origin this template may reach: its API, then its output origins (which output
    /// links may point to, and which never receive the credential). What a grant of its network
    /// capability is a grant of.
    pub fn origins(&self) -> impl Iterator<Item = &Origin> {
        std::iter::once(&self.origin).chain(&self.output_origins)
    }
}

/// What went wrong, without the input serde quotes: a template is someone else's text, and an
/// unknown field or variant could be a key pasted in the wrong place.
fn short(e: &serde_json::Error) -> String {
    let what = match e.classify() {
        serde_json::error::Category::Syntax | serde_json::error::Category::Io => "it isn't JSON",
        serde_json::error::Category::Eof => "it ends early",
        serde_json::error::Category::Data => "a field is missing, unknown or of the wrong type",
    };
    format!("{what} (line {}, column {})", e.line(), e.column())
}

fn check(raw: RawTemplate) -> Result<Template> {
    if raw.schema != TEMPLATE_SCHEMA {
        return Err(TemplateError::new("/schema", "this host reads provider template schema 1"));
    }
    let label = raw.label.trim();
    if label.is_empty() || label.len() > MAX_LABEL || label.chars().any(char::is_control) {
        return Err(TemplateError::new("/label", "a label is 1 to 64 bytes, with no control characters"));
    }
    let origin = Origin::parse(&raw.origin).map_err(|m| TemplateError::new("/origin", m))?;
    if raw.output_origins.len() > MAX_OUTPUT_ORIGINS {
        return Err(TemplateError::new("/output_origins", "at most 4 output origins"));
    }
    let mut output_origins = Vec::with_capacity(raw.output_origins.len());
    for (i, text) in raw.output_origins.iter().enumerate() {
        let o = Origin::parse(text).map_err(|m| TemplateError::new(format!("/output_origins/{i}"), m))?;
        if o == origin || output_origins.contains(&o) {
            return Err(TemplateError::new(format!("/output_origins/{i}"), "an origin is listed twice"));
        }
        output_origins.push(o);
    }
    if raw.models.is_empty() || raw.models.len() > MAX_MODELS {
        return Err(TemplateError::new("/models", "a template serves 1 to 64 models"));
    }
    let submit = submit(raw.submit)?;
    let poll = raw.poll.map(poll).transpose()?;
    let fields = match &submit.reply {
        SubmitReply::Finished { outputs } => outputs.as_slice(),
        SubmitReply::Queued { .. } => poll.as_ref().map_or(&[][..], |p| p.outputs.as_slice()),
    };
    let text = fields.iter().all(|f| f.kind == OutputKind::Text);
    let mut models: Vec<TemplateModel> = Vec::with_capacity(raw.models.len());
    for (i, m) in raw.models.into_iter().enumerate() {
        let at = format!("/models/{i}");
        if !model_id(&m.id) {
            return Err(TemplateError::new(format!("{at}/id"), "a model ID is up to 128 bytes of letters, digits, '.', '_', '-' and '/'-separated parts"));
        }
        if models.iter().any(|seen| seen.id == m.id) {
            return Err(TemplateError::new(format!("{at}/id"), "a model is listed twice"));
        }
        if m.price.micros == 0 {
            return Err(TemplateError::new(format!("{at}/price"), "a broker model has a positive price per job; it sends a credential to a paid service"));
        }
        let label = match m.label.as_deref().map(str::trim) {
            None => m.id.clone(),
            Some(l) if !l.is_empty() && l.len() <= MAX_LABEL && !l.chars().any(char::is_control) => l.to_owned(),
            Some(_) => return Err(TemplateError::new(format!("{at}/label"), "a label is 1 to 64 bytes, with no control characters")),
        };
        let output = match m.output {
            None if text => Modality::Text,
            None => Modality::Image,
            Some(o @ (Modality::Image | Modality::Video | Modality::Audio | Modality::Mesh | Modality::Text)) => o,
            Some(_) => return Err(TemplateError::new(format!("{at}/output"), "a model makes text, an image, a video, audio or a mesh")),
        };
        // Text is read from the reply; a file is downloaded from a link.
        if (output == Modality::Text) != text {
            return Err(TemplateError::new(format!("{at}/output"), "a text model has only text outputs, any other only links"));
        }
        models.push(TemplateModel { id: m.id, label, output, price: m.price });
    }
    match (&submit.reply, &poll) {
        (SubmitReply::Queued { .. }, None) => return Err(TemplateError::new("/poll", "a queued submit needs a poll operation")),
        (SubmitReply::Finished { .. }, Some(_)) => return Err(TemplateError::new("/poll", "a submit that finishes in its reply has nothing to poll")),
        _ => {}
    }
    let cancel = match raw.cancel {
        Some(c) if poll.is_some() => Some(CancelOperation { method: c.method, path: path(&c.path, true, "/cancel/path")? }),
        Some(_) => return Err(TemplateError::new("/cancel", "only a queued job can be cancelled")),
        None => None,
    };
    let l = raw.limits;
    if l.response_bytes == 0 || l.response_bytes > MAX_RESPONSE_BYTES {
        return Err(TemplateError::new("/limits/response_bytes", "a reply is 1 byte to 4 MiB"));
    }
    if l.timeout_ms < 1_000 || l.timeout_ms > MAX_TIMEOUT_MS {
        return Err(TemplateError::new("/limits/timeout_ms", "a request's timeout is 1 to 120 seconds"));
    }
    if l.concurrency == 0 || l.concurrency > MAX_CONCURRENCY {
        return Err(TemplateError::new("/limits/concurrency", "1 to 4 requests at once"));
    }
    let limits = Limits { response_bytes: l.response_bytes, timeout_ms: l.timeout_ms, concurrency: l.concurrency };
    Ok(Template {
        label: label.to_owned(),
        origin,
        output_origins,
        credential: raw.credential,
        idempotency: raw.idempotency,
        models,
        submit,
        poll,
        cancel,
        limits,
    })
}

fn model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MODEL_ID
        && id.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && part.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)))
}

/// A name of a parameter or a text role: `[a-z0-9_]`, starting with a letter.
fn name(text: &str) -> bool {
    text.len() <= MAX_NAME
        && text.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A path such as `/v1/{model}/jobs/{job}`: `{job}` only where a job exists (poll, cancel) and
/// required there; literal parts of unreserved characters only, with no empty, `.` or `..`
/// segment and no query.
fn path(text: &str, job: bool, at: &str) -> Result<PathTemplate> {
    let bad = |m: &str| TemplateError::new(at, m);
    if !text.starts_with('/') || text.len() > MAX_PATH {
        return Err(bad("a path starts with '/' and is at most 256 bytes"));
    }
    let mut has_job = false;
    for segment in text.split('/').skip(1) {
        match segment {
            "{model}" => {}
            "{job}" if job => has_job = true,
            "{job}" => return Err(bad("only poll and cancel paths name the job")),
            "" | "." | ".." => return Err(bad("a path has no empty, '.' or '..' segment")),
            s if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~:".contains(&b)) => {}
            _ => return Err(bad("a path segment is letters, digits, '-', '.', '_', '~', ':', or a whole {model} or {job}")),
        }
    }
    if job && !has_job {
        return Err(bad("a poll or cancel path names the job as {job}"));
    }
    Ok(PathTemplate(text.to_owned()))
}

fn pointer(text: &str, at: &str) -> Result<String> {
    let ok = text.len() <= MAX_POINTER && text.starts_with('/') && !text.chars().any(char::is_control);
    if ok { Ok(text.to_owned()) } else { Err(TemplateError::new(at, "a response field is a JSON pointer of at most 128 bytes")) }
}

fn outputs(raw: Vec<RawOutput>, at: &str) -> Result<Vec<OutputField>> {
    if raw.is_empty() || raw.len() > MAX_OUTPUTS {
        return Err(TemplateError::new(at, "1 to 16 output fields"));
    }
    raw.into_iter()
        .enumerate()
        .map(|(i, o)| {
            let at = format!("{at}/{i}");
            let mime = match o.mime {
                Some(m) if mime(&m) => Some(m),
                Some(_) => return Err(TemplateError::new(format!("{at}/mime"), "a MIME type is type/subtype, lowercase")),
                None => None,
            };
            Ok(OutputField { at: pointer(&o.at, &format!("{at}/at"))?, kind: o.kind, mime })
        })
        .collect()
}

fn mime(text: &str) -> bool {
    let mut parts = text.splitn(2, '/');
    let part = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+-.".contains(&b)));
    text.len() <= 64 && part(parts.next()) && part(parts.next())
}

fn submit(raw: RawSubmit) -> Result<SubmitOperation> {
    if raw.method != "POST" {
        return Err(TemplateError::new("/submit/method", "a submit is POST"));
    }
    let path = path(&raw.path, false, "/submit/path")?;
    let mut placeholders = 0;
    body(&raw.body, 0, &mut placeholders, "/submit/body")?;
    let reply = match (raw.handle, raw.outputs) {
        (Some(handle), None) => SubmitReply::Queued { handle: pointer(&handle, "/submit/handle")? },
        (None, Some(o)) => SubmitReply::Finished { outputs: outputs(o, "/submit/outputs")? },
        _ => return Err(TemplateError::new("/submit", "a submit reply has either a job handle or outputs")),
    };
    Ok(SubmitOperation { path, body: raw.body, reply })
}

/// What a body string stands for.
pub(crate) enum Placeholder<'a> {
    Model,
    Param(&'a str),
    Text(&'a str),
}

/// The placeholder a body string is, if it is one; `Err` for one this host doesn't know.
pub(crate) fn placeholder(text: &str) -> std::result::Result<Option<Placeholder<'_>>, ()> {
    let Some(inner) = text.strip_prefix('{').and_then(|t| t.strip_suffix('}')) else { return Ok(None) };
    match inner.split_once(':') {
        None if inner == "model" => Ok(Some(Placeholder::Model)),
        Some(("param", n)) if name(n) => Ok(Some(Placeholder::Param(n))),
        Some(("text", n)) if name(n) => Ok(Some(Placeholder::Text(n))),
        _ => Err(()),
    }
}

fn body(value: &Value, depth: usize, placeholders: &mut usize, at: &str) -> Result<()> {
    if depth > MAX_BODY_DEPTH {
        return Err(TemplateError::new(at, "a request body nests at most 12 deep"));
    }
    match value {
        Value::String(s) => match placeholder(s) {
            Ok(Some(_)) => {
                *placeholders += 1;
                if *placeholders > MAX_PLACEHOLDERS {
                    return Err(TemplateError::new(at, "a request body has at most 64 placeholders"));
                }
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(()) => Err(TemplateError::new(at, "a placeholder is {model}, {param:NAME} or {text:ROLE}")),
        },
        Value::Array(items) => items.iter().enumerate().try_for_each(|(i, v)| body(v, depth + 1, placeholders, &format!("{at}/{i}"))),
        Value::Object(map) => map.iter().try_for_each(|(k, v)| body(v, depth + 1, placeholders, &format!("{at}/{}", k.replace('~', "~0").replace('/', "~1")))),
        _ => Ok(()),
    }
}

fn states(raw: Vec<String>, at: &str, seen: &mut BTreeSet<String>) -> Result<BTreeSet<String>> {
    if raw.len() > MAX_STATES {
        return Err(TemplateError::new(at, "at most 16 status values per state"));
    }
    let mut out = BTreeSet::new();
    for (i, s) in raw.into_iter().enumerate() {
        if s.is_empty() || s.len() > MAX_STATUS || s.chars().any(char::is_control) {
            return Err(TemplateError::new(format!("{at}/{i}"), "a status value is 1 to 64 bytes"));
        }
        if !seen.insert(s.clone()) {
            return Err(TemplateError::new(format!("{at}/{i}"), "a status value is in two states"));
        }
        out.insert(s);
    }
    Ok(out)
}

fn poll(raw: RawPoll) -> Result<PollOperation> {
    if raw.method != "GET" {
        return Err(TemplateError::new("/poll/method", "a poll is GET"));
    }
    let mut seen = BTreeSet::new();
    let s = raw.states;
    let states = States {
        queued: states(s.queued, "/poll/states/queued", &mut seen)?,
        running: states(s.running, "/poll/states/running", &mut seen)?,
        done: states(s.done, "/poll/states/done", &mut seen)?,
        failed: states(s.failed, "/poll/states/failed", &mut seen)?,
    };
    if states.done.is_empty() || states.failed.is_empty() {
        return Err(TemplateError::new("/poll/states", "a poll names its done and failed statuses"));
    }
    if !(MIN_POLL_MS..=MAX_POLL_MS).contains(&raw.interval_ms) {
        return Err(TemplateError::new("/poll/interval_ms", "a poll interval is 0.5 to 60 seconds"));
    }
    Ok(PollOperation {
        path: path(&raw.path, true, "/poll/path")?,
        status: pointer(&raw.status, "/poll/status")?,
        states,
        outputs: outputs(raw.outputs, "/poll/outputs")?,
        error: raw.error.map(|e| pointer(&e, "/poll/error")).transpose()?,
        interval_ms: raw.interval_ms,
    })
}
