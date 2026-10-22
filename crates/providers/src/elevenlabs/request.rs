//! Each model's request, from the job's prompt or recording and its validated parameters.
use super::*;
use crate::{HttpRequest, JobInput};
use serde_json::{Map, Value, json};

/// The paid request, and the characters it bills when the model is priced by them.
pub(super) struct Prepared {
    pub(super) request: HttpRequest,
    pub(super) characters: Option<u64>,
}

/// Longest text each speech model takes in one request (ElevenLabs models page, 2026-10-21).
fn speech_chars(model: &str) -> Option<usize> {
    match model {
        V3 => Some(5_000),
        MULTILINGUAL_V2 => Some(10_000),
        FLASH_V2_5 => Some(40_000),
        _ => None,
    }
}
const SPEECH_TIMEOUT_MS: u64 = 300_000;
// Ten minutes of music can take minutes to compose.
const MUSIC_TIMEOUT_MS: u64 = 600_000;
const VOICE_ID_CHARS: usize = 64;
const MAX_SEED: u64 = 4_294_967_295;
/// Recordings the voice changer takes, as sniffed.
const RECORDINGS: &[(&str, &str)] =
    &[("audio/mpeg", "mp3"), ("audio/wav", "wav"), ("audio/ogg", "ogg"), ("audio/flac", "flac"), ("audio/mp4", "m4a"), ("audio/aac", "aac")];

pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared, ProviderError> {
    let p = &job.params;
    if let Some(max) = speech_chars(&job.model) {
        let text = prompt(job)?;
        let n = text.chars().count();
        if n > max {
            return Err(invalid(format!("ElevenLabs {} takes at most {max} characters; this text has {n}", job.model)));
        }
        let mut body = Map::new();
        body.insert("text".into(), text.into());
        body.insert("model_id".into(), job.model.as_str().into());
        body.insert("voice_settings".into(), settings(p, Some(&job.model))?);
        if let Some(seed) = seed(p)? {
            body.insert("seed".into(), seed.into());
        }
        // Multilingual v2 detects the language itself and refuses a code.
        if job.model != MULTILINGUAL_V2
            && let Some(code) = language(p)?
        {
            body.insert("language_code".into(), code.into());
        }
        let url = format!("{API}/v1/text-to-speech/{}?output_format={OUTPUT_FORMAT}", voice(p)?);
        let request = HttpRequest::post_json(url, &Value::Object(body))?.timeout_ms(SPEECH_TIMEOUT_MS);
        return Ok(Prepared { request, characters: Some(u64::try_from(n).map_err(|_| invalid("ElevenLabs text is too long"))?) });
    }
    let request = match job.model.as_str() {
        SOUND_EFFECTS => {
            let body = json!({
                "text": description(job, l)?,
                "model_id": SOUND_EFFECTS,
                "duration_seconds": number(p, "duration_seconds", 0.5, 30.0, 5.0)?,
                "prompt_influence": number(p, "prompt_influence", 0.0, 1.0, 0.3)?,
                "loop": flag(p, "loop")?,
            });
            HttpRequest::post_json(format!("{API}/v1/sound-generation?output_format={OUTPUT_FORMAT}"), &body)?.timeout_ms(SPEECH_TIMEOUT_MS)
        }
        MUSIC => {
            let seconds = match p.get("duration") {
                None => 30,
                Some(v) => v.as_u64().filter(|s| (3..=600).contains(s)).ok_or_else(|| invalid("ElevenLabs music lasts 3 to 600 seconds"))?,
            };
            let body = json!({
                "prompt": description(job, l)?,
                "music_length_ms": seconds.saturating_mul(1000),
                "model_id": MUSIC,
                "force_instrumental": flag(p, "force_instrumental")?,
            });
            HttpRequest::post_json(format!("{API}/v1/music?output_format={OUTPUT_FORMAT}"), &body)?.timeout_ms(MUSIC_TIMEOUT_MS)
        }
        VOICE_CHANGER => {
            let (mime, bytes) = recording(job, l)?;
            let ext = RECORDINGS.iter().find(|(m, _)| *m == mime).map_or("bin", |(_, e)| e);
            let name = format!("input.{ext}");
            let settings = serde_json::to_string(&settings(p, None)?).map_err(|_| invalid("ElevenLabs voice settings could not be encoded"))?;
            let denoise = if flag(p, "remove_background_noise")? { "true" } else { "false" };
            let seed = seed(p)?.map(|s| s.to_string());
            let mut fields = vec![("model_id", VOICE_CHANGER), ("voice_settings", settings.as_str()), ("remove_background_noise", denoise)];
            if let Some(seed) = &seed {
                fields.push(("seed", seed.as_str()));
            }
            let url = format!("{API}/v1/speech-to-speech/{}?output_format={OUTPUT_FORMAT}", voice(p)?);
            HttpRequest::post_multipart(url, &fields, &[("audio", name.as_str(), mime, bytes)])?.timeout_ms(SPEECH_TIMEOUT_MS)
        }
        _ => return Err(invalid("ElevenLabs has no such model in this adapter")),
    };
    Ok(Prepared { request, characters: None })
}

fn prompt(job: &ProviderJob) -> Result<&str, ProviderError> {
    let mut texts = job.inputs.iter().filter_map(|i| match i {
        JobInput::Text { role, text } if role == "prompt" => Some(text.as_str()),
        _ => None,
    });
    let text = texts.next().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| invalid("ElevenLabs needs a text prompt"))?;
    if texts.next().is_some() {
        return Err(invalid("ElevenLabs takes one text prompt"));
    }
    Ok(text)
}
fn description(job: &ProviderJob, l: Limits) -> Result<&str, ProviderError> {
    let text = prompt(job)?;
    if text.chars().count() > l.description_chars {
        return Err(invalid(format!("ElevenLabs descriptions are at most {} characters", l.description_chars)));
    }
    Ok(text)
}
/// The one recording, checked by its bytes rather than its claimed type.
fn recording(job: &ProviderJob, l: Limits) -> Result<(&'static str, &[u8]), ProviderError> {
    let mut media = job.inputs.iter().filter_map(|i| match i {
        JobInput::Media { role, bytes, .. } if role == "audio" => Some(bytes),
        _ => None,
    });
    let bytes = media.next().ok_or_else(|| invalid("ElevenLabs voice changer needs a recording"))?;
    if media.next().is_some() {
        return Err(invalid("ElevenLabs voice changer takes one recording"));
    }
    if bytes.len() > l.input_bytes {
        return Err(invalid(format!("ElevenLabs recordings are at most {} MiB here", l.input_bytes / MIB)));
    }
    let mime = nodecraft_media::sniff(bytes)
        .map(|s| s.mime)
        .filter(|m| RECORDINGS.iter().any(|(r, _)| r == m))
        .ok_or_else(|| invalid("ElevenLabs voice changer takes MP3, WAV, OGG, FLAC, M4A or AAC audio"))?;
    Ok((mime, bytes))
}
/// The voice id that goes into the URL path: the override when given, else the chosen voice.
/// Only ASCII letters and digits, so it can't change the path or query.
fn voice(p: &Map<String, Value>) -> Result<&str, ProviderError> {
    let id = text(p, "voice_id")?.or(text(p, "voice")?).ok_or_else(|| invalid("ElevenLabs needs a voice"))?;
    if id.len() > VOICE_ID_CHARS || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(invalid("ElevenLabs voice ids are letters and digits only"));
    }
    Ok(id)
}
fn language(p: &Map<String, Value>) -> Result<Option<&str>, ProviderError> {
    let Some(code) = text(p, "language_code")? else { return Ok(None) };
    if !(2..=3).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err(invalid("ElevenLabs language codes are two or three lowercase ISO 639 letters, like \"en\""));
    }
    Ok(Some(code))
}
/// Voice settings; `speech` is the text-to-speech model, `None` for the voice changer.
fn settings(p: &Map<String, Value>, speech: Option<&str>) -> Result<Value, ProviderError> {
    let mut s = Map::new();
    let stability = number(p, "stability", 0.0, 1.0, 0.5)?;
    // Eleven v3 takes only its three named stabilities: creative, natural and robust.
    if speech == Some(V3) && ![0.0, 0.5, 1.0].contains(&stability) {
        return Err(invalid("Eleven v3 stability is 0 (creative), 0.5 (natural) or 1 (robust)"));
    }
    s.insert("stability".into(), stability.into());
    s.insert("similarity_boost".into(), number(p, "similarity_boost", 0.0, 1.0, 0.75)?.into());
    if speech.is_some() {
        s.insert("style".into(), number(p, "style", 0.0, 1.0, 0.0)?.into());
        s.insert("speed".into(), number(p, "speed", 0.7, 1.2, 1.0)?.into());
    }
    Ok(Value::Object(s))
}
/// A non-empty, trimmed text parameter; empty means unset.
fn text<'a>(p: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>, ProviderError> {
    match p.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim()).filter(|s| !s.is_empty())),
        Some(_) => Err(invalid(format!("ElevenLabs parameter {name} must be text"))),
    }
}
fn number(p: &Map<String, Value>, name: &str, min: f64, max: f64, default: f64) -> Result<f64, ProviderError> {
    let Some(v) = p.get(name) else { return Ok(default) };
    v.as_f64()
        .filter(|n| n.is_finite() && (min..=max).contains(n))
        .ok_or_else(|| invalid(format!("ElevenLabs parameter {name} must be a number from {min} to {max}")))
}
fn flag(p: &Map<String, Value>, name: &str) -> Result<bool, ProviderError> {
    match p.get(name) {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or_else(|| invalid(format!("ElevenLabs parameter {name} must be on or off"))),
    }
}
fn seed(p: &Map<String, Value>) -> Result<Option<u64>, ProviderError> {
    match p.get("seed") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().filter(|s| *s <= MAX_SEED).map(Some).ok_or_else(|| invalid("ElevenLabs seeds are 0 to 4294967295")),
    }
}
