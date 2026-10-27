use super::*;
use crate::HttpResponse;
use serde::{Deserialize, de::Error as _};
use std::{borrow::Cow, marker::PhantomData};

struct Bounded<T, const N: usize>(Vec<T>);
impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for Bounded<T, N> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T, const N: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Visitor<T, N> {
            type Value = Bounded<T, N>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded array")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                if seq.size_hint().is_some_and(|n| n > N) {
                    return Err(A::Error::custom("array exceeds limit"));
                }
                let mut values = Vec::new();
                values.try_reserve_exact(N).map_err(|_| A::Error::custom("array allocation refused"))?;
                loop {
                    if values.len() == N {
                        return match seq.next_element::<serde::de::IgnoredAny>()? {
                            None => Ok(Bounded(values)),
                            Some(_) => Err(A::Error::custom("array exceeds limit")),
                        };
                    }
                    match seq.next_element()? {
                        Some(value) => values.push(value),
                        None => return Ok(Bounded(values)),
                    }
                }
            }
        }
        deserializer.deserialize_seq(Visitor::<T, N>(PhantomData))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    #[serde(borrow)]
    data: RawTask<'a>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask<'a> {
    #[serde(borrow)]
    task_id: Cow<'a, str>,
    #[serde(borrow)]
    status: Cow<'a, str>,
    #[serde(borrow)]
    generated: Bounded<Cow<'a, str>, 1>,
}
pub(super) enum State {
    Created,
    Running,
    Complete,
    Failed,
}
pub(super) struct Task<'a> {
    pub id: Cow<'a, str>,
    pub state: State,
    pub url: Option<Cow<'a, str>>,
}
fn delivery(url: &str, key: &Secret) -> Result<(), ProviderError> {
    if url.len() > 4096
        || !url.starts_with("https://ai-statics.freepik.com/")
        || url.contains(key.expose())
        || url.contains('\\')
        || url.chars().any(char::is_control)
    {
        return Err(malformed("Magnific image URL refused or redacted"));
    }
    let parsed = url::Url::parse(url).map_err(|_| malformed("Magnific image URL refused"))?;
    if parsed.as_str() != url
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("ai-statics.freepik.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.fragment().is_some()
    {
        return Err(malformed("Magnific image URL refused"));
    }
    Ok(())
}
pub(super) fn task<'a>(bytes: &'a [u8], key: &Secret) -> Result<Task<'a>, ProviderError> {
    // Body and HTTP metadata are bounded before this typed parse. Derived visitors refuse
    // duplicate/unknown envelope fields; the output array refuses a second entry before owning it.
    let raw: Envelope<'a> = serde_json::from_slice(bytes).map_err(|_| malformed("Magnific task response is not the documented envelope"))?;
    let raw = raw.data;
    identity(&raw.task_id)?;
    if raw.task_id.contains(key.expose()) {
        return Err(malformed("Magnific task identity refused or redacted"));
    }
    let state = match raw.status.as_ref() {
        "CREATED" => State::Created,
        "IN_PROGRESS" => State::Running,
        "COMPLETED" => State::Complete,
        "FAILED" => State::Failed,
        _ => return Err(malformed("Magnific task status is unknown")),
    };
    let url = if matches!(state, State::Complete) {
        let mut files = raw.generated.0.into_iter();
        let url = files.next().ok_or_else(|| malformed("Magnific complete task needs exactly one image"))?;
        delivery(&url, key)?;
        Some(url)
    } else {
        if !raw.generated.0.is_empty() {
            return Err(malformed("Magnific unfinished or failed task contains contradictory images"));
        }
        None
    };
    Ok(Task { id: raw.task_id, state, url })
}
pub(super) fn bounds(reply: &HttpResponse, cap: usize) -> Result<(), ProviderError> {
    if reply.body.len() > cap || reply.headers.len() > 128 {
        return Err(malformed("Magnific HTTP reply exceeds limits"));
    }
    let mut metadata = 0usize;
    let mut length = None;
    for (name, value) in &reply.headers {
        metadata = metadata
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.len()))
            .filter(|n| *n <= 64 * 1024)
            .ok_or_else(|| malformed("Magnific HTTP metadata exceeds limit"))?;
        if name.eq_ignore_ascii_case("content-length") {
            let n = value.parse::<usize>().map_err(|_| malformed("Magnific Content-Length refused"))?;
            if length.replace(n).is_some() || n != reply.body.len() {
                return Err(malformed("Magnific Content-Length disagrees with its body"));
            }
        }
    }
    Ok(())
}
fn retry_after(reply: &HttpResponse) -> Option<u64> {
    let mut values = reply.headers.iter().filter(|(name, _)| name.eq_ignore_ascii_case("retry-after"));
    let value = values.next()?.1.parse::<u64>().ok()?.checked_mul(1000)?;
    (values.next().is_none() && value <= 3_600_000).then_some(value)
}
pub(super) fn status(reply: &HttpResponse, cap: usize) -> Result<(), ProviderError> {
    bounds(reply, cap)?;
    let kind = match reply.status {
        200 => return Ok(()),
        400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Budget,
        404 => ErrorKind::NotFound,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        503 => ErrorKind::Busy,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Malformed,
    };
    Err(ProviderError { kind, message: format!("Magnific request failed with HTTP {}", reply.status), retry_after_ms: retry_after(reply) })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message<'a> {
    #[serde(borrow)]
    message: Cow<'a, str>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProblemEnvelope<'a> {
    #[serde(borrow)]
    problem: Problem<'a>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Problem<'a> {
    #[serde(borrow)]
    message: Cow<'a, str>,
    #[serde(borrow)]
    invalid_params: Bounded<InvalidParam<'a>, 32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvalidParam<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>,
    #[serde(borrow)]
    field: Cow<'a, str>,
    #[serde(borrow)]
    reason: Cow<'a, str>,
}
fn text_ok(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 4096
}
pub(super) fn refusal(reply: &HttpResponse, limits: Limits) -> bool {
    if !matches!(reply.status, 400 | 401) || bounds(reply, limits.response_bytes).is_err() {
        return false;
    }
    let mut types = reply.headers.iter().filter(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    if let Some((_, value)) = types.next() {
        let mime = value.split(';').next().unwrap_or_default().trim();
        if types.next().is_some()
            || !(mime.eq_ignore_ascii_case("application/json") || reply.status == 400 && mime.eq_ignore_ascii_case("application/problem+json"))
        {
            return false;
        }
    }
    if let Ok(message) = serde_json::from_slice::<Message<'_>>(&reply.body) {
        return text_ok(&message.message);
    }
    reply.status == 400
        && serde_json::from_slice::<ProblemEnvelope<'_>>(&reply.body).is_ok_and(|envelope| {
            text_ok(&envelope.problem.message)
                && envelope.problem.invalid_params.0.iter().all(|param| text_ok(&param.name) && text_ok(&param.field) && text_ok(&param.reason))
        })
}
