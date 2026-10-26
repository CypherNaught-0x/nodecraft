use super::*;
use crate::{HttpRequest, JobInput};
use serde_json::{Value, json};

const VOICES: &[&str] = &["alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer", "verse", "marin", "cedar"];

pub(super) fn prepare(job: &ProviderJob) -> Result<HttpRequest, ProviderError> {
    match job.model.as_str() {
        SPEECH => speech(job),
        TRANSCRIBE => transcription(job),
        _ => Err(invalid("OpenAI audio does not support that model; Realtime models cannot use the speech endpoint")),
    }
}
fn text_param<'a>(job: &'a ProviderJob, name: &str, max: usize) -> Result<Option<&'a str>, ProviderError> {
    match job.params.get(name) {
        None => Ok(None),
        Some(Value::String(text)) if text.len() <= max => Ok(Some(text)),
        _ => Err(invalid("OpenAI audio text parameter has the wrong type or exceeds its limit")),
    }
}
fn speech(job: &ProviderJob) -> Result<HttpRequest, ProviderError> {
    let [JobInput::Text { role, text }] = job.inputs.as_slice() else {
        return Err(invalid("OpenAI speech requires exactly one text prompt"));
    };
    if role != "prompt" || text.trim().is_empty() || text.chars().take(4097).count() > 4096 {
        return Err(invalid("OpenAI speech requires a nonempty prompt of at most 4096 characters"));
    }
    let voice = text_param(job, "voice", 32)?.unwrap_or("coral");
    if !VOICES.contains(&voice) {
        return Err(invalid("choose one of the supported OpenAI built-in voices"));
    }
    let speed = match job.params.get("speed") {
        None => 1.0,
        Some(value) => {
            value.as_f64().filter(|n| n.is_finite() && (0.25..=4.0).contains(n)).ok_or_else(|| invalid("OpenAI speech speed must be between 0.25 and 4"))?
        }
    };
    let mut body = json!({"model": SPEECH, "input": text, "voice": voice, "speed": speed, "response_format": "mp3", "stream_format": "audio"});
    if let Some(instructions) = text_param(job, "instructions", 16_384)? {
        if instructions.chars().take(4097).count() > 4096 {
            return Err(invalid("OpenAI speech instructions are at most 4096 characters"));
        }
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
    }
    HttpRequest::post_json(format!("{ROOT}/speech"), &body)
}
fn transcription(job: &ProviderJob) -> Result<HttpRequest, ProviderError> {
    let mut recording = None;
    let mut prompt = None;
    for input in &job.inputs {
        match input {
            JobInput::Media { role, mime, bytes } if role == "audio" && recording.is_none() => recording = Some((mime.as_str(), bytes.as_ref())),
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() && text.len() <= 16_384 => prompt = Some(text.as_str()),
            _ => return Err(invalid("OpenAI transcription takes one audio input and at most one short context prompt")),
        }
    }
    let (declared, bytes) = recording.ok_or_else(|| invalid("OpenAI transcription needs an audio input"))?;
    if bytes.is_empty() || bytes.len() > INPUT_BYTES {
        return Err(invalid("OpenAI transcription accepts a recording up to 25 MB"));
    }
    let sniffed = nodecraft_media::sniff(bytes).ok_or_else(|| invalid("the recording has no recognized audio header"))?;
    let ext = match sniffed.mime {
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        _ => return Err(invalid("OpenAI transcription currently accepts MP3, WAV, FLAC, Ogg or M4A audio here")),
    };
    if declared != sniffed.mime {
        return Err(invalid("the recording's declared type does not match its audio header"));
    }
    let mut fields = vec![("model", TRANSCRIBE), ("response_format", "json")];
    if let Some(prompt) = prompt.filter(|text| !text.trim().is_empty()) {
        fields.push(("prompt", prompt));
    }
    let request = HttpRequest::post_multipart(format!("{ROOT}/transcriptions"), &fields, &[("file", &format!("recording.{ext}"), sniffed.mime, bytes)])?;
    // The shared encoder avoids collisions in file bytes. This adapter also accepts arbitrary
    // text, so refuse a collision there before sending rather than letting it create a part.
    if let Some(boundary) =
        request.headers.iter().find(|(name, _)| name == "Content-Type").and_then(|(_, value)| value.strip_prefix("multipart/form-data; boundary="))
        && prompt.is_some_and(|text| text.contains(boundary))
    {
        return Err(invalid("the transcription context cannot be encoded safely"));
    }
    Ok(request)
}
