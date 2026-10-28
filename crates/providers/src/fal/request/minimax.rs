//! Current MiniMax contracts through the existing queue; no native API wire fields are mixed in.
use super::*;

pub(in crate::fal) fn valid_duration(model: Model, ms: u32) -> bool {
    match model {
        Model::MiniMaxText | Model::MiniMaxFrames => (5000..=15000).contains(&ms) && ms.is_multiple_of(1000),
        Model::MiniMaxMusic => (1000..=300000).contains(&ms) && ms.is_multiple_of(1000),
        Model::MiniMaxSpeech => ms == 3_600_000,
        _ => false,
    }
}
#[derive(Serialize)]
struct Video<'a> {
    prompt: &'a str,
    duration: u64,
    resolution: &'a str,
    prompt_expansion_mode: &'static str,
    enable_safety_checker: bool,
    sync_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
}
#[derive(Serialize)]
struct AudioSetting {
    format: &'static str,
    sample_rate: u32,
    bitrate: u32,
    channel: u8,
}
#[derive(Serialize)]
struct Voice<'a> {
    voice_id: &'a str,
    speed: f64,
    pitch: i64,
    vol: u8,
    english_normalization: bool,
}
#[derive(Serialize)]
struct Speech<'a> {
    prompt: &'a str,
    voice_setting: Voice<'a>,
    audio_setting: AudioSetting,
    output_format: &'static str,
    enable_safety_checker: bool,
}
#[derive(Serialize)]
struct Music<'a> {
    prompt: &'a str,
    lyrics: &'a str,
    duration: u64,
    guidance_scale: f64,
    num_inference_steps: u8,
    enable_safety_checker: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
}
fn encode(body: &impl Serialize, l: Limits) -> Result<Vec<u8>, ProviderError> {
    let mut count = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut count, body).map_err(|_| invalid("MiniMax escaped request exceeds limit"))?;
    let mut out = Vec::new();
    reserve(&mut out, count.n)?;
    serde_json::to_writer(&mut out, body).map_err(|_| invalid("MiniMax request encoding refused"))?;
    Ok(out)
}

fn geometry(l: Limits, base: u64, width: u64, height: u64) -> Result<(), ProviderError> {
    let short = base.div_ceil(64).checked_mul(64).ok_or_else(|| invalid("MiniMax geometry overflow"))?;
    let denominator = width.min(height);
    if denominator == 0 {
        return Err(invalid("MiniMax geometry denominator refused"));
    }
    let long = base
        .checked_mul(width.max(height))
        .map(|n| n.div_ceil(denominator))
        .and_then(|n| n.checked_add(63))
        .and_then(|n| n.checked_div(64))
        .and_then(|n| n.checked_mul(64))
        .ok_or_else(|| invalid("MiniMax geometry overflow"))?;
    let (w, h) = if width >= height { (long, short) } else { (short, long) };
    if w > u64::from(l.output_width) || h > u64::from(l.output_height) || w.checked_mul(h).is_none_or(|n| n > l.total_pixels) {
        return Err(invalid("MiniMax requested video exceeds lowered geometry limits before payment"));
    }
    Ok(())
}

fn seed(j: &ProviderJob) -> Result<Option<u64>, ProviderError> {
    j.params.get("seed").map(|v| v.as_u64().filter(|n| *n <= u64::from(u32::MAX)).ok_or_else(|| invalid("MiniMax local fixed seed must fit u32"))).transpose()
}
pub(in crate::fal) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() > 2 || j.params.len() > 4 || j.idempotency_key.len() > 256 {
        return Err(invalid("MiniMax input or identity count exceeds limit"));
    }
    let mut prompt = None;
    let mut image = None;
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                strings = strings.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("MiniMax needs one prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if model != Model::MiniMaxFrames
                    || role != "first_frame"
                    || image.replace((mime.as_str(), bytes.as_ref())).is_some()
                    || bytes.len() > l.input_bytes
                    || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp")
                {
                    return Err(invalid("MiniMax first frame must be one bounded PNG, JPEG or WebP"));
                }
            }
        }
    }
    for (key, value) in &j.params {
        strings = strings.saturating_add(key.len()).saturating_add(value.as_str().map_or(0, str::len));
        let allowed = match model {
            Model::MiniMaxText => matches!(key.as_str(), "duration" | "resolution" | "aspect_ratio" | "seed"),
            Model::MiniMaxFrames => matches!(key.as_str(), "duration" | "resolution" | "seed"),
            Model::MiniMaxSpeech => matches!(key.as_str(), "voice" | "speed" | "pitch"),
            Model::MiniMaxMusic => matches!(key.as_str(), "duration" | "lyrics" | "seed"),
            _ => false,
        };
        if !allowed || matches!(value, serde_json::Value::Array(_) | serde_json::Value::Object(_) | serde_json::Value::Null) {
            return Err(invalid("MiniMax unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("MiniMax text metadata exceeds limit"));
    }
    let prompt = prompt.filter(|s| !s.trim().is_empty()).ok_or_else(|| invalid("MiniMax prompt is required"))?;
    let cap = if model == Model::MiniMaxSpeech { 4096 } else { 8192 };
    if prompt.len() > cap {
        return Err(invalid("MiniMax prompt exceeds local text limit"));
    }
    let mut record = Record { model, requested: 1, width: 0, height: 0, duration_ms: 0 };
    if model == Model::MiniMaxSpeech {
        // Long pause directives make a short input an hours-long result; this plain speech slice excludes them before payment.
        if prompt.contains("<#") {
            return Err(invalid("MiniMax plain speech does not accept pause directives; remove <#...#> markup"));
        }
        record.duration_ms = 3_600_000;
        let voice_id = choice(j, "voice", "Wise_Woman", &["Wise_Woman", "Friendly_Person", "Deep_Voice_Man", "Calm_Woman"])?;
        let pitch = j
            .params
            .get("pitch")
            .map(|v| v.as_i64().filter(|n| (-12..=12).contains(n)).ok_or_else(|| invalid("MiniMax voice pitch must be -12 to 12")))
            .transpose()?
            .unwrap_or(0);
        return Ok((
            record,
            encode(
                &Speech {
                    prompt,
                    voice_setting: Voice { voice_id, speed: number(j, "speed", 1., 0.5, 2.)?, pitch, vol: 1, english_normalization: false },
                    audio_setting: AudioSetting { format: "mp3", sample_rate: 32000, bitrate: 128000, channel: 1 },
                    output_format: "url",
                    enable_safety_checker: true,
                },
                l,
            )?,
        ));
    }
    if model == Model::MiniMaxMusic {
        let lyrics = j.params.get("lyrics").map(|v| v.as_str().ok_or_else(|| invalid("MiniMax lyrics must be text"))).transpose()?.unwrap_or("[instrumental]");
        if lyrics.len() > 16384 {
            return Err(invalid("MiniMax lyrics exceed local 16 KiB limit"));
        }
        for line in lyrics.lines().map(str::trim) {
            if line.starts_with('[')
                && let Some((_, tail)) = line.split_once(']')
                && !tail.trim().is_empty()
            {
                return Err(invalid("MiniMax lyrics section tags must be on their own line; move the following words to the next line"));
            }
        }
        let duration = integer(j, "duration", 60, 1, 300)?;
        let pcm = duration.checked_mul(176400).and_then(|n| n.checked_add(65536)).ok_or_else(|| invalid("MiniMax WAV extent overflow"))?;
        if pcm > l.output_bytes as u64 || pcm > l.total_output_bytes as u64 {
            return Err(invalid("MiniMax WAV would exceed lowered output quota before payment"));
        }
        record.duration_ms = u32::try_from(duration * 1000).map_err(|_| invalid("MiniMax duration refused"))?;
        return Ok((
            record,
            encode(&Music { prompt, lyrics, duration, guidance_scale: 1.7, num_inference_steps: 30, enable_safety_checker: true, seed: seed(j)? }, l)?,
        ));
    }
    let frames = model == Model::MiniMaxFrames;
    if frames != image.is_some() {
        return Err(invalid("MiniMax frame video needs exactly one first frame"));
    }
    let duration = integer(j, "duration", 5, 5, 15)?;
    record.duration_ms = u32::try_from(duration * 1000).map_err(|_| invalid("MiniMax duration refused"))?;
    let mut body = Video {
        prompt,
        duration,
        resolution: choice(j, "resolution", "480P", &["480P", "768P"])?,
        prompt_expansion_mode: "disabled",
        enable_safety_checker: true,
        sync_mode: false,
        aspect_ratio: if frames { None } else { Some(choice(j, "aspect_ratio", "16:9", &["21:9", "16:9", "4:3", "1:1", "3:4", "9:16"])?) },
        image_url: image.map(|_| ""),
        seed: seed(j)?,
    };
    if let Some(aspect) = body.aspect_ratio {
        let (w, h) = aspect.split_once(':').ok_or_else(|| invalid("MiniMax aspect ratio refused"))?;
        geometry(
            l,
            if body.resolution == "480P" { 480 } else { 768 },
            w.parse().map_err(|_| invalid("MiniMax aspect ratio refused"))?,
            h.parse().map_err(|_| invalid("MiniMax aspect ratio refused"))?,
        )?;
    }
    let mut count = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut count, &body).map_err(|_| invalid("MiniMax escaped request exceeds limit"))?;
    let encoded_len = image.map(|(mime, bytes)| encoded(mime, bytes)).transpose()?;
    let extent = count
        .n
        .checked_add(encoded_len.unwrap_or(0))
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("MiniMax encoded frame exceeds request limit"))?;
    if let Some((mime, bytes)) = image {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("MiniMax frame geometry refused"))?;
        if meta.mime != mime
            || meta.width < 32
            || meta.height < 32
            || meta.width > meta.height.saturating_mul(3)
            || meta.height > meta.width.saturating_mul(3)
            || nodecraft_media::image_requires_orientation(bytes, &l.image(true)).map_err(|_| invalid("MiniMax frame orientation refused"))?
        {
            return Err(invalid("MiniMax frame must be upright, 32–1024 px per axis, with aspect ratio between 1:3 and 3:1"));
        }
        geometry(l, if body.resolution == "480P" { 480 } else { 768 }, u64::from(meta.width), u64::from(meta.height))?;
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("MiniMax frame codec refused"))?);
    }
    let image_uri = image.zip(encoded_len).map(|((mime, bytes), n)| uri(mime, bytes, n)).transpose()?;
    body.image_url = image_uri.as_deref();
    let out = encode(&body, l)?;
    if out.len() != extent {
        return Err(invalid("MiniMax request extent changed"));
    }
    Ok((record, out))
}
