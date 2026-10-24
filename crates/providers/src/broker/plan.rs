//! Requests a template allows, and the response fields it may read. Pure: nothing here sends.
//!
//! A job fills only the body's placeholders; the URL is the template's origin and path, with the
//! model the template lists and a job id the host checked. A response is read only at the
//! template's pointers, and an output URL must be on one of its origins.

use serde_json::{Map, Value, json};

use super::template::{OutputField, OutputKind, PathTemplate, Placeholder, SubmitReply, Template, placeholder};
use crate::error::excerpt;
use crate::{Completed, ErrorKind, HttpMethod, HttpRequest, JobHandle, JobInput, JobProgress, MAX_STAGED_BODY, Output, ProviderError, ProviderJob, Submission};

const MAX_JOB_ID: usize = 128;
const MAX_KEY: usize = 128;

fn unsupported(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}

fn malformed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}

/// A job id from a reply: it goes into a path, so only unreserved characters.
fn job_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_JOB_ID && id != "." && id != ".." && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

fn url(template: &Template, path: &PathTemplate, model: &str, job: Option<&str>) -> String {
    let mut out = String::from(template.origin.as_str());
    for segment in path.0.split('/').skip(1) {
        out.push('/');
        match segment {
            "{model}" => out.push_str(model),
            "{job}" => out.push_str(job.unwrap_or_default()),
            s => out.push_str(s),
        }
    }
    out
}

fn bounded(template: &Template, request: HttpRequest) -> HttpRequest {
    request.timeout_ms(template.limits.timeout_ms).max_response_bytes(template.limits.response_bytes)
}

/// The values a job fills placeholders with.
struct Fill<'a> {
    model: &'a str,
    params: &'a Map<String, Value>,
    texts: Vec<(&'a str, &'a str)>,
}

impl Fill<'_> {
    fn value(&self, text: &str) -> Result<Option<Value>, ProviderError> {
        match placeholder(text) {
            Ok(Some(Placeholder::Model)) => Ok(Some(Value::String(self.model.to_owned()))),
            Ok(Some(Placeholder::Param(n))) => Ok(self.params.get(n).cloned()),
            Ok(Some(Placeholder::Text(role))) => Ok(self.texts.iter().find(|(r, _)| *r == role).map(|(_, t)| Value::String((*t).to_owned()))),
            Ok(None) => Ok(Some(Value::String(text.to_owned()))),
            Err(()) => Err(malformed("the provider template has a placeholder this host doesn't know")),
        }
    }

    /// The body with every placeholder filled. A member whose placeholder the job doesn't set
    /// is left out; an array element can't be, so it is an error.
    fn body(&self, value: &Value) -> Result<Value, ProviderError> {
        Ok(match value {
            Value::String(s) => self.value(s)?.ok_or_else(|| unsupported("the job is missing a value the provider template needs"))?,
            Value::Array(items) => Value::Array(items.iter().map(|v| self.body(v)).collect::<Result<_, _>>()?),
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, v) in map {
                    let filled = match v {
                        Value::String(s) => self.value(s)?,
                        other => Some(self.body(other)?),
                    };
                    if let Some(filled) = filled {
                        out.insert(k.clone(), filled);
                    }
                }
                Value::Object(out)
            }
            other => other.clone(),
        })
    }
}

/// The submit request for `job`, without its credential (see [`authorize`]).
pub fn plan_submit(template: &Template, job: &ProviderJob) -> Result<HttpRequest, ProviderError> {
    let model = template.model(&job.model).ok_or_else(|| unsupported("this plugin provider doesn't serve that model"))?;
    let mut texts: Vec<(&str, &str)> = Vec::with_capacity(job.inputs.len());
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if texts.iter().any(|(r, _)| r == role) => {
                return Err(unsupported(format!("a plugin provider takes one {role} text")));
            }
            JobInput::Text { role, text } => texts.push((role, text)),
            JobInput::Media { .. } => return Err(unsupported("plugin providers take only text inputs yet")),
        }
    }
    let fill = Fill { model: &model.id, params: &job.params, texts };
    let body = fill.body(&template.submit.body)?;
    let mut request = HttpRequest::post_json(url(template, &template.submit.path, &model.id, None), &body)?;
    if request.body.len() > MAX_STAGED_BODY {
        return Err(unsupported("the request for this job is larger than 1 MiB"));
    }
    if let Some(header) = template.idempotency {
        let key = &job.idempotency_key;
        if key.is_empty() || key.len() > MAX_KEY || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(unsupported("this job has no idempotency key the provider can take"));
        }
        request = request.header(header.name(), key.clone());
    }
    Ok(bounded(template, request))
}

fn handle_parts<'a>(template: &'a Template, handle: &'a JobHandle) -> Result<(&'a str, &'a str), ProviderError> {
    let model = handle
        .data
        .get("model")
        .and_then(Value::as_str)
        .and_then(|m| template.model(m))
        .ok_or_else(|| malformed("the job's handle names no model of its provider"))?;
    if !job_id(&handle.remote_id) {
        return Err(malformed("the job's id isn't one this provider's paths can take"));
    }
    Ok((&model.id, &handle.remote_id))
}

/// The poll request for `handle`, without its credential.
pub fn plan_poll(template: &Template, handle: &JobHandle) -> Result<HttpRequest, ProviderError> {
    let poll = template.poll.as_ref().ok_or_else(|| unsupported("this plugin provider's jobs finish in their reply"))?;
    let (model, job) = handle_parts(template, handle)?;
    Ok(bounded(template, HttpRequest::get(url(template, &poll.path, model, Some(job)))))
}

/// The cancel request for `handle`, without its credential; `None` when the template can't cancel.
pub fn plan_cancel(template: &Template, handle: &JobHandle) -> Result<Option<HttpRequest>, ProviderError> {
    let Some(cancel) = &template.cancel else { return Ok(None) };
    let (model, job) = handle_parts(template, handle)?;
    let mut request = HttpRequest::get(url(template, &cancel.path, model, Some(job)));
    request.method = match cancel.method {
        super::template::CancelMethod::Post => HttpMethod::Post,
        super::template::CancelMethod::Delete => HttpMethod::Delete,
    };
    Ok(Some(bounded(template, request)))
}

/// `request` with the credential where the template places it. Refused unless the request goes to
/// the template's API origin: never its output origins, which may be a store the package's
/// publisher runs. The credential is marked secret, so it never follows a redirect to another
/// origin or shows in a log.
pub fn authorize(template: &Template, request: HttpRequest, key: &str) -> Result<HttpRequest, ProviderError> {
    let on_origin = url::Url::parse(&request.url).is_ok_and(|u| template.origin.holds(&u) && u.username().is_empty() && u.password().is_none());
    if !on_origin {
        return Err(unsupported("a plugin provider's credential goes only to its API origin"));
    }
    Ok(match template.credential {
        Some(credential) => {
            let (name, value) = credential.header(key);
            request.secret_header(name, value)
        }
        None => request,
    })
}

fn parse(template: &Template, body: &[u8]) -> Result<Value, ProviderError> {
    if body.len() > template.limits.response_bytes {
        return Err(malformed("the provider's reply is larger than its template allows"));
    }
    serde_json::from_slice(body).map_err(|_| malformed("the provider's reply isn't JSON"))
}

fn outputs(template: &Template, reply: &Value, fields: &[OutputField]) -> Result<Vec<Output>, ProviderError> {
    let mut out = Vec::new();
    for field in fields {
        let values: Vec<&str> = match reply.pointer(&field.at) {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(items)) => {
                items.iter().map(|v| v.as_str().ok_or_else(|| malformed("an output in the provider's reply isn't text"))).collect::<Result<_, _>>()?
            }
            _ => return Err(malformed("the provider's reply has no output where its template says")),
        };
        for value in values {
            if out.len() >= super::template::MAX_OUTPUTS {
                return Err(malformed("the provider's reply has more than 16 outputs"));
            }
            out.push(match field.kind {
                OutputKind::Text => Output::Text(value.to_owned()),
                OutputKind::Url => {
                    let parsed = url::Url::parse(value).map_err(|_| malformed("an output link in the provider's reply isn't a URL"))?;
                    if !parsed.username().is_empty() || parsed.password().is_some() {
                        return Err(malformed("an output link in the provider's reply carries a user name"));
                    }
                    if !template.origins().any(|o| o.holds(&parsed)) {
                        return Err(malformed("an output link in the provider's reply is outside its template's origins"));
                    }
                    Output::Url { url: parsed.to_string(), mime: field.mime.clone() }
                }
            });
        }
    }
    if out.is_empty() {
        return Err(malformed("the provider's reply has no outputs"));
    }
    Ok(out)
}

/// Reads a successful submit reply for `model`.
pub fn read_submit(template: &Template, model: &str, body: &[u8]) -> Result<Submission, ProviderError> {
    let model = template.model(model).ok_or_else(|| unsupported("this plugin provider doesn't serve that model"))?;
    let reply = parse(template, body)?;
    match &template.submit.reply {
        SubmitReply::Queued { handle } => {
            let id = reply
                .pointer(handle)
                .and_then(Value::as_str)
                .filter(|id| job_id(id))
                .ok_or_else(|| malformed("the provider's reply has no job id this host can follow"))?;
            Ok(Submission::Queued(JobHandle { remote_id: id.to_owned(), data: json!({"model": model.id}) }))
        }
        SubmitReply::Finished { outputs: fields } => Ok(Submission::Finished(Completed::new(outputs(template, &reply, fields)?))),
    }
}

/// Reads a successful poll reply. A failure message that holds `secret` (the credential; empty
/// for none) isn't quoted: an excerpt could cut it and keep only its start.
pub fn read_poll(template: &Template, body: &[u8], secret: &str) -> Result<JobProgress, ProviderError> {
    let poll = template.poll.as_ref().ok_or_else(|| unsupported("this plugin provider's jobs finish in their reply"))?;
    let reply = parse(template, body)?;
    let status = reply.pointer(&poll.status).and_then(Value::as_str).ok_or_else(|| malformed("the provider's reply has no status where its template says"))?;
    let s = &poll.states;
    Ok(if s.done.contains(status) {
        JobProgress::Done(Completed::new(outputs(template, &reply, &poll.outputs)?))
    } else if s.failed.contains(status) {
        let said = poll
            .error
            .as_ref()
            .and_then(|p| reply.pointer(p))
            .and_then(Value::as_str)
            .filter(|m| secret.is_empty() || !m.contains(secret))
            .map(|m| excerpt(m.as_bytes()));
        JobProgress::Failed(ProviderError::new(ErrorKind::JobFailed, said.unwrap_or_else(|| "the provider says the job failed".into())))
    } else if s.queued.contains(status) {
        JobProgress::Queued { position: None }
    } else {
        // Running, or a status the template doesn't list: the job's deadline still bounds it, and
        // failing a paid job on a new status would lose what it is producing.
        JobProgress::Running { fraction: None, message: None, partial: None }
    })
}
