use super::*;
use crate::{HttpResponse, Output};
use serde::Deserialize;

pub(super) fn transport(error: ProviderError) -> ProviderError {
    ProviderError {
        kind: error.kind,
        message: "Stability AI transport failed; prior processing or billing may be uncertain".into(),
        retry_after_ms: error.retry_after_ms.filter(|n| *n <= 3_600_000),
    }
}
pub(super) fn bounds(reply: &HttpResponse, cap: usize) -> Result<(), ProviderError> {
    if reply.body.len() > cap || reply.headers.len() > 128 {
        return Err(malformed());
    }
    let mut bytes = 0usize;
    let mut length = None;
    for (name, value) in &reply.headers {
        bytes = bytes.checked_add(name.len()).and_then(|n| n.checked_add(value.len())).filter(|n| *n <= METADATA).ok_or_else(malformed)?;
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.len() > 20 || !value.bytes().all(|c| c.is_ascii_digit()) {
                return Err(malformed());
            }
            length = Some(value.parse::<usize>().map_err(|_| malformed())?);
        }
    }
    if length.is_some_and(|length| length != reply.body.len()) {
        return Err(malformed());
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    id: String,
    name: String,
    errors: Vec<String>,
}
/// Only a bounded, documented error envelope clears paid-send evidence. Never a 5xx or redirect.
pub(super) fn refusal(reply: &HttpResponse) -> Option<ErrorKind> {
    if !matches!(reply.status, 400 | 401 | 402 | 403 | 413 | 422 | 429) || reply.body.len() > METADATA {
        return None;
    }
    let value: Refusal = serde_json::from_slice(&reply.body).ok()?;
    if value.id.is_empty()
        || value.id.len() > 128
        || value.name.is_empty()
        || value.name.len() > 128
        || value.errors.is_empty()
        || value.errors.len() > 32
        || value.errors.iter().any(|s| s.is_empty() || s.len() > 8192)
    {
        return None;
    }
    match reply.status {
        400 | 413 | 422 => Some(ErrorKind::UnsupportedInput),
        401 => Some(ErrorKind::Auth),
        402 => Some(ErrorKind::Budget),
        403 if value.name == "content_moderation" => Some(ErrorKind::BlockedContent),
        429 => Some(ErrorKind::RateLimited),
        _ => None,
    }
}
pub(super) fn status(reply: &HttpResponse, wanted: u16) -> Result<(), ProviderError> {
    if reply.status == wanted {
        return Ok(());
    }
    let kind = refusal(reply)
        .unwrap_or_else(|| if (200..300).contains(&reply.status) { ErrorKind::Malformed } else { ProviderError::from_status(reply.status, b"").kind });
    let mut error = ProviderError::new(kind, format!("Stability AI request failed (HTTP {})", reply.status));
    error.retry_after_ms = reply
        .header("retry-after")
        .filter(|value| value.len() <= 16)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    Err(error)
}
pub(super) fn media(reply: HttpResponse, model: Model, duration: Option<u16>) -> Result<Completed, ProviderError> {
    if reply.headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("finish-reason") && value != "SUCCESS") {
        return Err(ProviderError::new(ErrorKind::BlockedContent, "Stability AI did not return an unfiltered successful result"));
    }
    let sniffed = nodecraft_media::sniff(&reply.body).ok_or_else(malformed)?;
    let expected = match model {
        Model::Core => {
            if !matches!(sniffed.mime, "image/png" | "image/jpeg" | "image/webp") {
                return Err(malformed());
            }
            let limits = nodecraft_media::Limits { max_input_bytes: IMAGE_BYTES as u64, max_width: 4096, max_height: 4096, max_alloc: 128 * MIB as u64 };
            drop(nodecraft_media::decode_image(&reply.body, &limits).map_err(|_| malformed())?);
            sniffed.mime
        }
        Model::Audio => {
            if sniffed.mime != "audio/wav" {
                return Err(malformed());
            }
            let extent = reply
                .body
                .get(4..8)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(u32::from_le_bytes)
                .and_then(|len| usize::try_from(len).ok())
                .and_then(|len| len.checked_add(8));
            let sound = nodecraft_media::probe_sound(&reply.body);
            let limit = duration.map_or(385_000, |seconds| u64::from(seconds).saturating_mul(1000).saturating_add(5000));
            if extent != Some(reply.body.len())
                || !sound.duration_ms.is_some_and(|n| (1..=limit).contains(&n))
                || sound.sample_rate != Some(44_100)
                || sound.channels != Some(2)
            {
                return Err(malformed());
            }
            "audio/wav"
        }
        Model::Mesh => {
            if sniffed.mime != "model/gltf-binary" {
                return Err(malformed());
            }
            drop(
                nodecraft_media::decode_mesh(&reply.body, &nodecraft_media::MeshLimits { max_input_bytes: MESH_BYTES, ..Default::default() })
                    .map_err(|_| malformed())?,
            );
            "model/gltf-binary"
        }
    };
    let mut types = reply.headers.iter().filter(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    if let Some((_, mime)) = types.next()
        && (mime.split(';').next().map(str::trim) != Some(expected) || types.next().is_some())
    {
        return Err(malformed());
    }
    Ok(Completed::new(vec![Output::Bytes { mime: expected.into(), bytes: reply.body.into() }]))
}
