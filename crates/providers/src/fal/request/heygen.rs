//! Supplied-audio Photo Avatar4 and explicit lip-sync schemas; no TTS or voice identifiers.
use super::*;
#[derive(Serialize)]
#[serde(untagged)]
enum Body<'a> {
    Photo {
        image_url: &'a str,
        audio_url: &'a str,
        talking_style: &'static str,
        resolution: &'static str,
        aspect_ratio: &'static str,
        caption: bool,
        enable_safety_checker: bool,
    },
    Lipsync {
        video_url: &'a str,
        audio_url: &'a str,
        enable_dynamic_duration: bool,
        enable_caption: bool,
        disable_music_track: bool,
        enable_speech_enhancement: bool,
    },
}
pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    let photo = model == Model::HeygenAvatar;
    if j.inputs.len() != 2 || j.idempotency_key.len() > 256 || j.params.len() > if photo { 0 } else { 2 } {
        return Err(invalid("HeyGen needs one photo or video and one supplied speech track"));
    }
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    let mut visual = None;
    let mut speech = None;
    for input in &j.inputs {
        let crate::JobInput::Media { role, mime, bytes } = input else {
            return Err(invalid("HeyGen accepts supplied media only; text-to-speech is not configured"));
        };
        strings = strings.saturating_add(role.len()).saturating_add(mime.len());
        let (target, allowed, cap) = match role.as_str() {
            "speech" => (&mut speech, mime == "audio/wav", 1024 * 1024),
            "portrait" if photo => (&mut visual, matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp"), 1024 * 1024),
            "video" if !photo => (&mut visual, mime == "video/mp4", 16 * 1024 * 1024),
            _ => return Err(invalid("HeyGen requires named portrait/video and speech inputs")),
        };
        if !allowed || bytes.is_empty() || bytes.len() > cap.min(l.input_bytes) || target.replace((mime.as_str(), bytes.as_ref())).is_some() {
            return Err(invalid("HeyGen input MIME, byte limit or duplicate role refused"));
        }
    }
    for (name, value) in &j.params {
        strings = strings.saturating_add(name.len());
        if photo || !matches!(name.as_str(), "disable_music_track" | "enable_speech_enhancement") || !value.is_boolean() {
            return Err(invalid("HeyGen accepts only the documented lip-sync boolean controls"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("HeyGen metadata exceeds limit"));
    }
    let (mime, bytes) = visual.ok_or_else(|| invalid("HeyGen photo/video required"))?;
    let (audio_mime, audio) = speech.ok_or_else(|| invalid("HeyGen speech WAV required"))?;
    // The vendor does not publish exact encoded-size/duration guarantees. New paid work
    // requires our complete response/output envelope; saved handles still intersect current caps.
    let hard = Limits::VIDEO_HARD;
    if l.output_width < hard.output_width
        || l.output_height < hard.output_height
        || l.total_pixels < hard.total_pixels
        || l.output_bytes < hard.output_bytes
        || l.total_output_bytes < hard.total_output_bytes
        || l.metadata_bytes < hard.metadata_bytes
        || l.response_bytes < hard.response_bytes
    {
        return Err(invalid("HeyGen submissions require the full local video response/output envelope"));
    }
    let music = j.params.get("disable_music_track").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let enhance = j.params.get("enable_speech_enhancement").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let body = |visual, speech| {
        if photo {
            Body::Photo {
                image_url: visual,
                audio_url: speech,
                talking_style: "stable",
                resolution: "720p",
                aspect_ratio: "16:9",
                caption: false,
                enable_safety_checker: true,
            }
        } else {
            Body::Lipsync {
                video_url: visual,
                audio_url: speech,
                enable_dynamic_duration: false,
                enable_caption: false,
                disable_music_track: music,
                enable_speech_enhancement: enhance,
            }
        }
    };
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body("", "")).map_err(|_| invalid("HeyGen escaped request exceeds limit"))?;
    let visual_len = encoded(mime, bytes)?;
    let audio_len = encoded(audio_mime, audio)?;
    let length = counter
        .n
        .checked_add(visual_len)
        .and_then(|n| n.checked_add(audio_len))
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("HeyGen encoded request exceeds limit"))?;
    // Both data URI lengths have been admitted before media work or owned encodings.
    media::heygen::audio(audio).map_err(|_| invalid("HeyGen requires a complete PCM WAV of at most ten seconds"))?;
    if photo {
        raster::check(mime, bytes)?;
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("HeyGen photo exceeds 1024×1024 raster limits"))?;
        if meta.mime != mime || nodecraft_media::image_requires_orientation(bytes, &l.image(true)).map_err(|_| invalid("HeyGen photo orientation refused"))? {
            return Err(invalid("HeyGen needs an upright photo with a matching raster MIME"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("HeyGen photo codec refused"))?);
    } else {
        media::heygen::input_video(bytes, l).map_err(|_| invalid("HeyGen requires a measured MP4 up to 1280×720 and ten seconds"))?;
    }
    let visual = uri(mime, bytes, visual_len)?;
    let audio = uri(audio_mime, audio, audio_len)?;
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body(&visual, &audio)).map_err(|_| invalid("HeyGen request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("HeyGen request extent changed"));
    }
    Ok((Record { model, requested: 1, width: 0, height: 0 }, out))
}
