//! `nodecraft-cli plugin try` (#866): runs one registration of a plugin package the way the app
//! would, with no key and no network, so an author can check it end to end in CI.
//!
//! - A **text action** runs on the given text inputs, through the same compile and run the app
//!   uses, and its outputs are reported.
//! - A **provider** template is sent by the real [`Broker`] over a scripted transport: each request
//!   it makes is answered by the next of the author's replies, and recorded (method, URL, header
//!   names, body). The broker's own rules apply unchanged: the credential only to the API origin,
//!   no redirects, outputs only from declared origins. The credential is a fixture, never a key.
//!
//! Nothing is installed, granted or enabled, and nothing leaves the machine.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use nodecraft_actions::expr::Value as Data;
use nodecraft_plugin::manifest::{RegistrationKind, Runtime};
use nodecraft_plugin::signature::TrustStore;
use nodecraft_plugin_store::{MemoryBackend, Store};
use nodecraft_providers::broker::{Broker, Template};
use nodecraft_providers::secrets::Secret;
use nodecraft_providers::{
    BoxFuture, CancelSignal, Clock, Http, HttpRequest, HttpResponse, JobInput, Output, ProviderError, ProviderJob, RunLimits, run_to_completion,
};
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

/// The most scripted replies one try takes.
pub const MAX_REPLIES: usize = 64;
/// The most body bytes of a request shown back; longer bodies are cut.
const SHOWN_BODY: usize = 4096;
/// What the fixture credential is: never a real key.
const FIXTURE_KEY: &str = "plugin-try-fixture-key";

/// One scripted reply to the broker's next request.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What to try: a registration, its inputs, and (for a provider) the model, its parameters and
/// the replies its requests get.
#[derive(Clone, Debug, Default)]
pub struct Try {
    pub registration: String,
    /// Text inputs by name: an action's inputs, or a provider job's text roles (`prompt`, …).
    pub inputs: BTreeMap<String, String>,
    pub model: Option<String>,
    pub params: serde_json::Map<String, Value>,
    pub replies: Vec<Reply>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Runs `what` in the package `bytes`, and reports what happened.
pub async fn try_package(bytes: &[u8], what: &Try) -> Result<Value, Error> {
    // Everything installing, granting and enabling would refuse is refused first, so a package
    // that tries cleanly is one the app takes.
    crate::plugin_cmds::lint(bytes, &crate::plugin_cmds::PluginHost { trust: None, taken: crate::plugin_cmds::host_ids()?, bindings: Default::default() })?;
    let store = Store::open(MemoryBackend::default())?;
    let package = store.inspect(bytes, &TrustStore::default())?;
    let manifest = &package.manifest;
    let position = manifest
        .registrations
        .iter()
        .position(|r| r.id.as_str() == what.registration)
        .ok_or_else(|| invalid(format!("the package registers no {}", what.registration)))?;
    match &manifest.runtime {
        Runtime::TextActions { .. } => {
            if what.model.is_some() || !what.params.is_empty() || !what.replies.is_empty() {
                return Err(invalid("a text action takes only its inputs: no model, parameters or replies"));
            }
            let actions = crate::plugin_text::compile(&package)?;
            let action = actions.get(position).ok_or_else(|| invalid("that registration didn't compile"))?;
            let inputs: BTreeMap<String, Data> = what.inputs.iter().map(|(k, v)| (k.clone(), Data::Text(v.clone()))).collect();
            let outputs = action
                .action
                .run(&inputs, &BTreeMap::new(), nodecraft_actions::expr::ExprLimits::default(), &|| false)
                .map_err(|e| invalid(format!("the action failed: {e}")))?;
            let outputs: serde_json::Map<String, Value> = outputs.into_iter().map(|(name, value)| (name, shown(&value))).collect();
            Ok(json!({"package_id": manifest.package_id.as_str(), "registration": what.registration, "kind": "action", "outputs": outputs}))
        }
        Runtime::ProviderOperations { .. } => {
            let registration = manifest.registrations.get(position).ok_or_else(|| invalid("that registration is missing"))?;
            if registration.kind != RegistrationKind::Provider {
                return Err(invalid("that registration isn't a provider"));
            }
            let descriptor = package.files.get(&registration.descriptor).ok_or_else(|| invalid("its template is missing"))?;
            let template = Template::decode(descriptor).map_err(|e| invalid(format!("its template can't be used: {e}")))?;
            try_provider(manifest.package_id.as_str(), &what.registration, template, what).await
        }
        Runtime::Data => Err(invalid("a data package runs nothing: there is nothing to try")),
        Runtime::RustWasm { .. } => Err(Error::new(ErrorKind::Unsupported, "module (rust_wasm) packages can't run in this NodeCraft")),
    }
}

async fn try_provider(package: &str, registration: &str, template: Template, what: &Try) -> Result<Value, Error> {
    if what.replies.len() > MAX_REPLIES {
        return Err(invalid(format!("at most {MAX_REPLIES} replies are scripted")));
    }
    let model = match &what.model {
        Some(model) => model.clone(),
        None => template.models.first().map(|m| m.id.clone()).ok_or_else(|| invalid("its template lists no model"))?,
    };
    let key = template.credential.is_some().then(|| Secret::new(FIXTURE_KEY)).transpose().map_err(|_| invalid("the fixture credential can't be made"))?;
    let http = Arc::new(Scripted { replies: Mutex::new(what.replies.iter().cloned().collect()), sent: Mutex::default() });
    let broker = Broker::new(format!("plugin.{package}"), template, key, http.clone()).map_err(|e| invalid(e.message))?;
    let job = ProviderJob {
        model,
        params: what.params.clone(),
        inputs: what.inputs.iter().map(|(role, text)| JobInput::Text { role: role.clone(), text: text.clone() }).collect(),
        idempotency_key: "plugin-try".into(),
    };
    let outcome = run_to_completion(&broker, &job, &Never, &Instant, RunLimits::default(), |_| {}).await;
    let outcome = match outcome {
        Ok(done) => json!({"outputs": done.outputs.iter().map(output).collect::<Vec<_>>()}),
        Err(failure) => json!({"error": failure.error.to_string(), "possibly_billed": failure.uncertain}),
    };
    let requests: Vec<Value> = lock(&http.sent).iter().map(request).collect();
    Ok(json!({"package_id": package, "registration": registration, "kind": "provider", "requests": requests, "outcome": outcome}))
}

/// An action's value as JSON.
fn shown(value: &Data) -> Value {
    match value {
        Data::Int(n) => json!(n),
        Data::Bool(b) => json!(b),
        Data::Text(text) => json!(text),
        Data::List(items) => Value::Array(items.iter().map(shown).collect()),
    }
}

fn output(output: &Output) -> Value {
    match output {
        Output::Text(text) => json!({"text": text}),
        Output::Bytes { mime, bytes } => json!({"mime": mime, "bytes": bytes.len()}),
        Output::Url { url, mime } => json!({"url": url, "mime": mime}),
    }
}

/// A request as the author sees it: its headers by name only, so the fixture credential's place
/// shows without its value, and its body cut to [`SHOWN_BODY`].
fn request(request: &HttpRequest) -> Value {
    let body = String::from_utf8_lossy(request.body.get(..request.body.len().min(SHOWN_BODY)).unwrap_or_default()).into_owned();
    let headers: Vec<&str> = request.headers.iter().map(|(name, _)| name.as_str()).collect();
    let method = format!("{:?}", request.method).to_ascii_uppercase();
    json!({"method": method, "url": request.url, "headers": headers, "body": body, "body_bytes": request.body.len(), "body_cut": request.body.len() > SHOWN_BODY})
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Answers each request with the next scripted reply; out of replies, the request fails as the
/// network would.
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    sent: Mutex<Vec<HttpRequest>>,
}

impl Scripted {
    fn answer(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        lock(&self.sent).push(request);
        let reply = lock(&self.replies).pop_front();
        reply
            .map(|r| HttpResponse { status: r.status, headers: r.headers, body: r.body })
            .ok_or_else(|| ProviderError::new(nodecraft_providers::ErrorKind::Network, "no scripted reply is left for this request"))
    }
}

impl Http for Scripted {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let answer = self.answer(request);
        Box::pin(async move { answer })
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.send(request)
    }
}

struct Never;
impl CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}

/// Time that passes at once: a try never waits for a poll interval.
struct Instant;
impl Clock for Instant {
    fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

#[cfg(test)]
#[path = "plugin_try/tests.rs"]
mod tests;
