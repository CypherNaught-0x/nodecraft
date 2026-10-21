//! Higgsfield's replies: `{detail}` errors, receipts, request status, upload grants, estimates,
//! the restyle preset list and Soul IDs.
use super::*;
use serde_json::Value;

fn json(body: &[u8]) -> Result<Value, ProviderError> {
    serde_json::from_slice(body).map_err(|_| malformed("Higgsfield reply isn't the expected JSON"))
}
/// Higgsfield's `detail`: a message, or a list of validation errors whose first message is kept.
pub(super) fn detail(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    match v.get("detail")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.first().and_then(|i| i.get("msg")).and_then(Value::as_str).map(|s| format!("invalid request: {s}")),
        _ => None,
    }
}
/// Higgsfield's message, cleaned and shortened, unless it echoes the key.
fn said(text: &str, leaks: impl Fn(&str) -> bool) -> String {
    if leaks(text) {
        return "(message withheld)".into();
    }
    text.chars().filter(|c| !c.is_control()).take(300).collect()
}
/// A 422 that says the key was used before with another body: the earlier request may have
/// been accepted and billed, and this one is not a new request.
fn reused_key(status: u16, detail: Option<&str>) -> bool {
    status == 422
        && detail.is_some_and(|d| {
            let d = d.to_ascii_lowercase();
            d.contains("idempotency") || d.contains("different parameters")
        })
}
/// Whether a submit was refused before Higgsfield accepted it (#631). Higgsfield documents that
/// a request rejected by authentication, validation, credit or model checks is never accepted
/// and doesn't use its key. Only those statuses with Higgsfield's `{detail}` body count; a 5xx,
/// an unreadable body or a reused key stays possibly billed.
pub(super) fn refused_submit(r: &HttpResponse) -> bool {
    let detail = detail(&r.body);
    detail.is_some() && matches!(r.status, 400 | 401 | 403 | 404 | 422 | 423 | 503) && !reused_key(r.status, detail.as_deref())
}
/// A reply that fits the limit, with a failing status turned into an error.
pub(super) fn check(r: &HttpResponse, l: Limits, leaks: impl Fn(&str) -> bool) -> Result<(), ProviderError> {
    if r.body.len() > l.response_bytes {
        return Err(malformed("Higgsfield reply exceeds the size limit"));
    }
    if r.is_success() {
        return Ok(());
    }
    let detail = detail(&r.body);
    let message = said(detail.as_deref().unwrap_or("no details"), &leaks);
    let lower = message.to_ascii_lowercase();
    let (kind, retry_ms, text) = match r.status {
        _ if reused_key(r.status, detail.as_deref()) => (
            ErrorKind::UnsupportedInput,
            None,
            "Higgsfield already took a request under this run's key with other inputs, most likely an earlier attempt whose reply was lost. It may have been billed; check Higgsfield before running again".to_owned(),
        ),
        400 if lower.contains("concurrent") => (ErrorKind::RateLimited, Some(15_000), format!("Higgsfield is running as many requests as this account allows: {message}")),
        400 | 422 => (ErrorKind::UnsupportedInput, None, format!("Higgsfield refused the request: {message}")),
        401 => (ErrorKind::Auth, None, "Higgsfield refused the key; check the key ID and secret".to_owned()),
        403 => (ErrorKind::Budget, None, format!("Higgsfield refused the request for credit: {message}")),
        404 => (ErrorKind::NotFound, None, format!("Higgsfield doesn't know that request or model on this account: {message}")),
        423 => (ErrorKind::Busy, Some(60_000), format!("Higgsfield has blocked this model for now: {message}")),
        503 => (ErrorKind::Busy, Some(30_000), format!("Higgsfield has this model disabled or not ready: {message}")),
        status => {
            let e = ProviderError::from_status(status, b"");
            (e.kind, None, format!("Higgsfield failed (HTTP {status}): {message}"))
        }
    };
    let mut error = ProviderError::new(kind, text);
    error.retry_after_ms = r
        .header("retry-after")
        .filter(|v| v.len() <= 16)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .or(retry_ms)
        .filter(|ms| *ms <= 3_600_000);
    Err(error)
}
pub(super) fn receipt(body: &[u8]) -> Result<String, ProviderError> {
    let v = json(body)?;
    let id = v.get("request_id").and_then(Value::as_str).ok_or_else(|| malformed("Higgsfield accepted no request id"))?;
    // Returned status and cancel links are ignored: the adapter only sends the key to links it
    // builds from the checked id, so a reply can't steer it, and an accepted, billed request is
    // never dropped over the shape of a link.
    request_id(id)?;
    // A replayed key may answer `queued` for a request that has moved on; polling reads the truth.
    match v.get("status").and_then(Value::as_str) {
        Some("queued" | "in_progress" | "completed" | "failed" | "nsfw" | "canceled") => Ok(id.to_owned()),
        _ => Err(malformed("Higgsfield receipt has an unknown status")),
    }
}
pub(super) enum Status {
    Progress(JobProgress),
    /// Finished: the result links, in Higgsfield's order.
    Done(Vec<String>),
}
fn link(v: &Value, leaks: &impl Fn(&str) -> bool) -> Result<String, ProviderError> {
    let url = v.get("url").and_then(Value::as_str).ok_or_else(|| malformed("Higgsfield result has no link"))?;
    if leaks(url) {
        return Err(malformed("Higgsfield result link refused"));
    }
    crate::model_file::public_link(url, "Higgsfield")
}
pub(super) fn status(body: &[u8], id: &str, model: &str, count: u64, leaks: impl Fn(&str) -> bool) -> Result<Status, ProviderError> {
    let v = json(body)?;
    if v.get("request_id").and_then(Value::as_str) != Some(id) {
        return Err(malformed("Higgsfield status is for another request"));
    }
    let failed = |kind, message: String| Ok(Status::Progress(JobProgress::Failed(ProviderError::new(kind, message))));
    let reason = || v.get("error").and_then(Value::as_str).map(|e| said(e, &leaks)).filter(|e| !e.is_empty());
    match v.get("status").and_then(Value::as_str).ok_or_else(|| malformed("Higgsfield status missing"))? {
        "queued" => Ok(Status::Progress(JobProgress::Queued { position: None })),
        "in_progress" => Ok(Status::Progress(JobProgress::Running { fraction: None, message: None, partial: None })),
        "completed" => {
            let urls = if model == SOUL {
                let images = v.get("images").and_then(Value::as_array).ok_or_else(|| malformed("Higgsfield finished without images"))?;
                if images.is_empty() || images.len() as u64 > count {
                    return Err(malformed("Higgsfield returned an unexpected number of images"));
                }
                images.iter().map(|i| link(i, &leaks)).collect::<Result<Vec<_>, _>>()?
            } else {
                vec![link(v.get("video").ok_or_else(|| malformed("Higgsfield finished without a video"))?, &leaks)?]
            };
            Ok(Status::Done(urls))
        }
        "failed" => failed(
            ErrorKind::JobFailed,
            match reason() {
                Some(why) => format!("Higgsfield couldn't make this: {why}"),
                None => "Higgsfield couldn't make this".into(),
            },
        ),
        "nsfw" => failed(ErrorKind::BlockedContent, "Higgsfield's moderation refused the input or the result; nothing was billed".into()),
        "canceled" => failed(ErrorKind::Cancelled, "the Higgsfield request was cancelled before it started".into()),
        _ => Err(malformed("Higgsfield reported an unknown status")),
    }
}
/// A presigned upload: where to put the bytes, the headers it requires, and the public link the
/// request then names.
pub(super) struct Grant {
    pub upload_url: String,
    pub public_url: String,
    pub headers: Vec<(String, String)>,
}
pub(super) fn upload_grant(body: &[u8], mime: &str, leaks: impl Fn(&str) -> bool) -> Result<Grant, ProviderError> {
    let v = json(body)?;
    let text = |name: &str| v.get(name).and_then(Value::as_str).ok_or_else(|| malformed("Higgsfield upload grant is incomplete"));
    if text("content_type")? != mime {
        return Err(malformed("Higgsfield upload grant is for another type"));
    }
    let (upload, public) = (text("upload_url")?, text("public_url")?);
    if leaks(upload) || leaks(public) {
        return Err(malformed("Higgsfield upload link refused"));
    }
    let upload_url = crate::model_file::public_link(upload, "Higgsfield")?;
    let public_url = crate::model_file::public_link(public, "Higgsfield")?;
    // Only the storage headers a presigned PUT needs; nothing that could carry a credential.
    let mut headers = Vec::new();
    match v.get("upload_headers") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) if map.len() <= 16 => {
            for (name, value) in map {
                let lower = name.to_ascii_lowercase();
                let value = value.as_str().ok_or_else(|| malformed("Higgsfield upload header refused"))?;
                let allowed = lower == "content-type" || lower.starts_with("x-amz-") || lower.starts_with("x-goog-");
                let clean = name.len() <= 64
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && value.len() <= 1024
                    && value.bytes().all(|b| b.is_ascii_graphic() || b == b' ');
                if !allowed || !clean || leaks(value) || (lower == "content-type" && value != mime) {
                    return Err(malformed("Higgsfield upload header refused"));
                }
                headers.push((name.clone(), value.to_owned()));
            }
        }
        Some(_) => return Err(malformed("Higgsfield upload headers refused")),
    }
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("content-type")) {
        headers.push(("Content-Type".into(), mime.to_owned()));
    }
    Ok(Grant { upload_url, public_url, headers })
}
/// A non-negative decimal of at most 12 digits before and after the point.
fn decimal(s: &str) -> Option<(&str, &str)> {
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    let digits = |p: &str, empty: bool| (empty || !p.is_empty()) && p.len() <= 12 && p.bytes().all(|b| b.is_ascii_digit());
    (digits(whole, false) && digits(fraction, true) && !(s.contains('.') && fraction.is_empty())).then_some((whole, fraction))
}
/// US dollars to micros, rounded up so a fraction of a micro is never free.
fn usd(s: &str) -> Option<u64> {
    let (whole, fraction) = decimal(s)?;
    let whole: u64 = whole.parse().ok()?;
    let mut micros: u64 = 0;
    for (i, b) in fraction.bytes().enumerate() {
        let digit = u64::from(b - b'0');
        if i < 6 {
            micros = micros.checked_add(digit.checked_mul(10u64.checked_pow(5 - i as u32)?)?)?;
        } else if digit > 0 {
            micros = micros.checked_add(1)?;
            break;
        }
    }
    whole.checked_mul(1_000_000)?.checked_add(micros)
}
pub(super) fn estimate(body: &[u8]) -> Result<Estimate, ProviderError> {
    let v = json(body)?;
    let credits = v.get("credits").and_then(Value::as_str).filter(|s| decimal(s).is_some()).ok_or_else(|| malformed("Higgsfield estimate has no credits"))?;
    let micros = v.get("usd").and_then(Value::as_str).and_then(usd).ok_or_else(|| malformed("Higgsfield estimate has no price"))?;
    Ok(Estimate { credits: credits.to_owned(), usd: nodecraft_types::Money::new(nodecraft_types::Currency::USD, micros) })
}
/// The downloaded result's type, from its bytes: an image from SOUL, an MP4 from the video models.
pub(super) fn media(bytes: &[u8], model: &str) -> Result<&'static str, ProviderError> {
    let sniffed = nodecraft_media::sniff(bytes).map(|s| s.mime);
    let found = match (model, sniffed) {
        (SOUL, Some("image/png")) => "image/png",
        (SOUL, Some("image/jpeg")) => "image/jpeg",
        (SOUL, Some("image/webp")) => "image/webp",
        (KLING_I2V | MOTION_TRANSFER | OBJECT_SWAP | RESTYLE, Some("video/mp4")) => "video/mp4",
        _ => return Err(malformed("Higgsfield returned a different kind of media than the model makes")),
    };
    Ok(found)
}
/// The most presets a list may have; Higgsfield lists a few dozen.
const MAX_PRESETS: usize = 1024;
/// The restyle preset list, `{items: [{id, name}]}`. An entry without a UUID id or a readable
/// name is left out and counted; a repeated id keeps its first entry.
pub(super) fn presets(body: &[u8], leaks: impl Fn(&str) -> bool) -> Result<Presets, ProviderError> {
    let v = json(body)?;
    let items = v.get("items").and_then(Value::as_array).ok_or_else(|| malformed("Higgsfield preset list has no items"))?;
    if items.len() > MAX_PRESETS {
        return Err(malformed("Higgsfield lists more presets than NodeCraft takes"));
    }
    let mut out = Presets::default();
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        let id = item.get("id").and_then(Value::as_str).filter(|id| request::uuid(id)).map(str::to_ascii_lowercase);
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty() && n.len() <= 256 && !n.chars().any(char::is_control) && !leaks(n));
        match (id, name) {
            (Some(id), Some(name)) if seen.insert(id.clone()) => out.presets.push(Preset { id, name: name.to_owned() }),
            _ => out.skipped = out.skipped.saturating_add(1),
        }
    }
    Ok(out)
}
/// A Soul ID's fields as Higgsfield documents its reference object; `None` for one that doesn't
/// read as one.
fn reference_fields<'a>(item: &'a Value, leaks: &impl Fn(&str) -> bool) -> Option<(String, &'a str, &'a str, &'a str)> {
    let id = item.get("id").and_then(Value::as_str).filter(|id| request::uuid(id))?.to_ascii_lowercase();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.chars().count() <= 100 && !n.chars().any(char::is_control) && !leaks(n))?;
    let version = item.get("model_version").and_then(Value::as_str).filter(|v| v.len() <= 32)?;
    let status = item.get("status").and_then(Value::as_str).filter(|s| s.len() <= 32)?;
    Some((id, name, version, status))
}
/// One Soul ID, read with `GET /v1/custom-references/{id}`: what SOUL version it was trained
/// for, and where its training stands.
pub(super) fn reference(body: &[u8], id: &str, leaks: impl Fn(&str) -> bool) -> Result<Reference, ProviderError> {
    let v = json(body)?;
    let (got, _, version, status) = reference_fields(&v, &leaks).ok_or_else(|| malformed("Higgsfield's Soul ID reply isn't the documented reference"))?;
    if got != id.to_ascii_lowercase() {
        return Err(malformed("Higgsfield answered for a different Soul ID"));
    }
    let training = match status {
        "completed" => Training::Completed,
        "not_ready" | "queued" | "in_progress" => Training::Pending,
        "failed" => Training::Failed(v.get("fail_reason").and_then(Value::as_str).filter(|r| !r.trim().is_empty()).map(|r| said(r, &leaks))),
        // An undocumented state is never taken for ready.
        _ => Training::Pending,
    };
    Ok(Reference { v2: version == "v2", training })
}
/// One page of `GET /v1/custom-references/list`, whose shape Higgsfield doesn't document: a
/// list of reference objects, bare or under `items` as its other lists are. Anything else is
/// refused rather than guessed at. Returns the ready SOUL 2 entries, how many entries the page
/// had, and how many didn't read as a reference.
pub(super) fn soul_page(body: &[u8], page_size: usize, leaks: impl Fn(&str) -> bool) -> Result<(Vec<SoulId>, usize, usize), ProviderError> {
    let v = json(body)?;
    let items = match &v {
        Value::Array(items) => items,
        _ => v.get("items").and_then(Value::as_array).ok_or_else(|| malformed("Higgsfield's Soul ID list isn't a list NodeCraft reads"))?,
    };
    if items.len() > page_size {
        return Err(malformed("Higgsfield's Soul ID list page is longer than asked for"));
    }
    let mut ready = Vec::new();
    let mut skipped = 0usize;
    for item in items {
        match reference_fields(item, &leaks) {
            Some((id, name, "v2", "completed")) => ready.push(SoulId { id, name: name.to_owned() }),
            // Another SOUL version's, or one still training or failed: real, but not usable here.
            Some(_) => {}
            None => skipped = skipped.saturating_add(1),
        }
    }
    Ok((ready, items.len(), skipped))
}
