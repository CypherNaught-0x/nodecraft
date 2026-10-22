//! Google Cloud's replies read into what the adapter acts on. Nothing here touches the network.
use super::*;
use base64::Engine as _;
use serde::Deserialize;

/// Where a fetched operation stands.
pub(super) enum Operation {
    Running,
    Done(Arc<[u8]>),
    Failed(ProviderError),
}

#[derive(Deserialize)]
struct Started {
    name: String,
}
#[derive(Deserialize)]
struct Status {
    code: Option<i64>,
    message: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Video {
    bytes_base64_encoded: Option<String>,
    gcs_uri: Option<String>,
    mime_type: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Generated {
    #[serde(default)]
    videos: Vec<Video>,
    #[serde(default)]
    rai_media_filtered_count: u64,
    #[serde(default)]
    rai_media_filtered_reasons: Vec<String>,
}
#[derive(Deserialize)]
struct Fetched {
    #[serde(default)]
    done: bool,
    error: Option<Status>,
    response: Option<Generated>,
}

/// A reply that isn't a success becomes the error a person can act on. The reply isn't quoted:
/// it may echo the request.
pub(super) fn check(reply: &HttpResponse, l: Limits, paid: bool, project: &str) -> Result<(), ProviderError> {
    if reply.body.len() > l.response_bytes {
        return Err(malformed("the Google Cloud reply exceeds its byte limit"));
    }
    if reply.is_success() {
        return Ok(());
    }
    let kind = match reply.status {
        300..=399 => ErrorKind::Malformed,
        400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
        401 | 403 => ErrorKind::Auth,
        404 | 410 => ErrorKind::NotFound,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        503 => ErrorKind::Busy,
        _ => ErrorKind::Server,
    };
    let message = match (paid, reply.status) {
        (_, 403) => format!(
            "Google Cloud refused the service account: check that the Vertex AI API is enabled in project {project} and the account has the Vertex AI User role"
        ),
        (_, 404) => format!("Google Cloud has no such Veo model for project {project} in {LOCATION}"),
        (_, 429) => "Google Cloud's Veo quota is used up for now; try again shortly".into(),
        (true, _) => "the Veo request failed; it may have been billed".into(),
        (false, _) => "checking on the Veo video failed".into(),
    };
    let mut error = ProviderError::new(kind, format!("{message} (HTTP {})", reply.status));
    error.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    Err(error)
}

fn segment_ok(s: &str) -> bool {
    (1..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// The model an operation name belongs to, if it is a Veo operation in this adapter's region:
/// `projects/<project>/locations/us-central1/publishers/google/models/<model>/operations/<id>`.
/// The project may be the number rather than the id.
pub(super) fn operation_model(name: &str) -> Result<&'static str, ProviderError> {
    let refused = || malformed("the Veo operation name isn't one this adapter started");
    let parts: Vec<&str> = name.split('/').collect();
    let ["projects", project, "locations", LOCATION, "publishers", "google", "models", model, "operations", id] = parts[..] else {
        return Err(refused());
    };
    if !segment_ok(project) || !segment_ok(id) {
        return Err(refused());
    }
    [GENERATE, FAST, LITE].into_iter().find(|m| *m == model).ok_or_else(refused)
}
pub(super) fn started(body: &[u8], model: &str) -> Result<String, ProviderError> {
    let reply: Started = serde_json::from_slice(body).map_err(|_| malformed("the Veo start reply can't be read; the video may have been billed"))?;
    if operation_model(&reply.name)? != model {
        return Err(malformed("the Veo operation belongs to another model; the video may have been billed"));
    }
    Ok(reply.name)
}

/// Google's support codes in a safety message (`… Support codes: 15236754, 64151117`), as the
/// categories its responsible-AI page names. Nothing else of the message is kept.
fn safety_categories(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() == 8)
        .filter_map(|code| {
            Some(match code {
                "58061214" | "17301594" => "child safety",
                "29310472" | "15236754" => "a celebrity's likeness",
                "64151117" | "42237218" => "video safety",
                "62263041" => "dangerous content",
                "57734940" | "22137204" => "hate",
                "89371032" | "49114662" | "63429089" | "72817394" | "60599140" => "prohibited content",
                "35561574" | "35561575" => "third-party content",
                "90789179" | "43188360" => "sexual content",
                "78610348" => "toxicity",
                "61493863" | "56562880" => "violence",
                "32635315" => "vulgarity",
                "74803281" | "29578790" | "42876398" => "other safety rules",
                _ => return None,
            })
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}
fn blocked(texts: &[&str]) -> ProviderError {
    let categories: Vec<&str> = texts.iter().flat_map(|t| safety_categories(t)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let why = if categories.is_empty() { String::new() } else { format!(" ({})", categories.join(", ")) };
    ProviderError::new(
        ErrorKind::BlockedContent,
        format!("Veo's safety filters blocked the video{why}; revise the prompt or images. Google doesn't say whether a blocked video is billed"),
    )
}

pub(super) fn operation(body: &[u8], l: Limits) -> Result<Operation, ProviderError> {
    let reply: Fetched = serde_json::from_slice(body).map_err(|_| malformed("the Veo operation can't be read"))?;
    if let Some(error) = reply.error {
        let message = error.message.unwrap_or_default();
        // google.rpc.Code 3, INVALID_ARGUMENT, carries the safety refusals.
        if !safety_categories(&message).is_empty() || message.contains("usage guidelines") {
            return Ok(Operation::Failed(blocked(&[&message])));
        }
        let code = error.code.map_or(String::new(), |c| format!(" (code {c})"));
        return Ok(Operation::Failed(ProviderError::new(ErrorKind::JobFailed, format!("Veo couldn't make the video{code}"))));
    }
    if !reply.done {
        return Ok(Operation::Running);
    }
    let generated = reply.response.ok_or_else(|| malformed("the finished Veo operation has neither a video nor an error"))?;
    let mut videos = generated.videos.into_iter();
    let video = match (videos.next(), videos.next()) {
        (Some(video), None) => video,
        (None, None) if generated.rai_media_filtered_count > 0 || !generated.rai_media_filtered_reasons.is_empty() => {
            let reasons: Vec<&str> = generated.rai_media_filtered_reasons.iter().take(8).map(String::as_str).collect();
            return Ok(Operation::Failed(blocked(&reasons)));
        }
        (None, None) => return Err(malformed("the finished Veo operation has no video")),
        _ => return Err(malformed("Veo returned more than one video")),
    };
    if video.mime_type.as_deref().is_some_and(|m| m != MP4) {
        return Err(malformed("Veo returned a video that isn't MP4"));
    }
    match (video.bytes_base64_encoded, video.gcs_uri) {
        (Some(data), None) => {
            // Base64 is four characters for every three bytes: refuse an oversize video unread.
            if data.len() / 4 * 3 > l.output_bytes {
                return Err(malformed("the Veo video exceeds the download limit"));
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|_| malformed("the Veo video data isn't base64"))?;
            if bytes.len() > l.output_bytes {
                return Err(malformed("the Veo video exceeds the download limit"));
            }
            if nodecraft_media::sniff(&bytes).map(|s| s.mime) != Some(MP4) {
                return Err(malformed("the Veo video isn't an MP4 file"));
            }
            Ok(Operation::Done(Arc::from(bytes)))
        }
        // No storage bucket is ever named, so the video should never be one.
        (None, Some(_)) => Err(malformed("Veo wrote the video to Cloud Storage instead of returning it")),
        _ => Err(malformed("the Veo video has neither data nor a location")),
    }
}
