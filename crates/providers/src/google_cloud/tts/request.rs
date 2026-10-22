//! Each model's request, from the job's prompt and its validated parameters.
use super::*;
use crate::JobInput;
use serde_json::{Map, Value, json};

/// The paid request's body, and the characters it bills when the model is priced by them.
pub(super) struct Prepared {
    pub(super) body: Value,
    pub(super) characters: Option<u64>,
}

/// Longest text or SSML a classic voice takes in one request.
const CLASSIC_BYTES: usize = 5_000;
/// Longest text, and longest style prompt, a Gemini-TTS request takes (each on its own).
const GEMINI_BYTES: usize = 4_000;
const VOICE_BYTES: usize = 64;
const LANGUAGE_BYTES: usize = 16;
const ALIAS_BYTES: usize = 32;
/// Gemini-TTS's prebuilt voices, which every Gemini-TTS model shares.
const GEMINI_VOICES: &[&str] = &[
    "Achernar",
    "Achird",
    "Algenib",
    "Algieba",
    "Alnilam",
    "Aoede",
    "Autonoe",
    "Callirrhoe",
    "Charon",
    "Despina",
    "Enceladus",
    "Erinome",
    "Fenrir",
    "Gacrux",
    "Iapetus",
    "Kore",
    "Laomedeia",
    "Leda",
    "Orus",
    "Pulcherrima",
    "Puck",
    "Rasalgethi",
    "Sadachbia",
    "Sadaltager",
    "Schedar",
    "Sulafat",
    "Umbriel",
    "Vindemiatrix",
    "Zephyr",
    "Zubenelgenubi",
];

/// A classic family: the segment its voice names carry, and what it takes besides the text.
struct Family {
    segment: &'static str,
    max_rate: f64,
    pitch: bool,
}
fn family(model: &str) -> Option<Family> {
    Some(match model {
        CHIRP3_HD => Family { segment: "Chirp3-HD", max_rate: 2.0, pitch: false },
        NEURAL2 => Family { segment: "Neural2", max_rate: 4.0, pitch: true },
        WAVENET => Family { segment: "Wavenet", max_rate: 4.0, pitch: true },
        STANDARD => Family { segment: "Standard", max_rate: 4.0, pitch: true },
        STUDIO => Family { segment: "Studio", max_rate: 4.0, pitch: false },
        _ => return None,
    })
}

pub(super) fn prepare(job: &ProviderJob) -> Result<Prepared, ProviderError> {
    let p = &job.params;
    let words = prompt(job)?;
    if let Some(family) = family(&job.model) {
        if words.len() > CLASSIC_BYTES {
            return Err(invalid(format!("Google TTS takes at most {CLASSIC_BYTES} bytes of text; this text has {}", words.len())));
        }
        let (name, language) = classic_voice(p, &family)?;
        let input = match text(p, "input_format")?.unwrap_or("text") {
            "text" => json!({"text": words}),
            "ssml" => json!({"ssml": words}),
            _ => return Err(invalid("Google TTS input is text or SSML")),
        };
        let mut audio = Map::new();
        audio.insert("audioEncoding".into(), "MP3".into());
        audio.insert("speakingRate".into(), number(p, "speaking_rate", 0.25, family.max_rate, 1.0)?.into());
        if family.pitch {
            audio.insert("pitch".into(), number(p, "pitch", -20.0, 20.0, 0.0)?.into());
        }
        let body = json!({"input": input, "voice": {"languageCode": language, "name": name}, "audioConfig": audio});
        // Google bills every character sent, SSML tags included.
        let characters = u64::try_from(words.chars().count()).map_err(|_| invalid("Google TTS text is too long"))?;
        return Ok(Prepared { body, characters: Some(characters) });
    }
    if ![GEMINI_FLASH, GEMINI_FLASH_LITE, GEMINI_PRO, GEMINI_3_1_FLASH].contains(&job.model.as_str()) {
        return Err(invalid("Google TTS has no such model in this adapter"));
    }
    if words.len() > GEMINI_BYTES {
        return Err(invalid(format!("Gemini-TTS takes at most {GEMINI_BYTES} bytes of text; this text has {}", words.len())));
    }
    let mut input = Map::new();
    input.insert("text".into(), words.into());
    if let Some(style) = text(p, "style")? {
        if style.len() > GEMINI_BYTES {
            return Err(invalid(format!("Gemini-TTS style prompts are at most {GEMINI_BYTES} bytes")));
        }
        input.insert("prompt".into(), style.into());
    }
    let mut voice = Map::new();
    voice.insert("languageCode".into(), language(p)?.into());
    voice.insert("modelName".into(), job.model.as_str().into());
    match speakers(p)? {
        Some(_) if job.model == GEMINI_FLASH_LITE => {
            return Err(invalid("Gemini 2.5 Flash-Lite TTS speaks with one voice; clear the speaker names or pick another model"));
        }
        Some(speakers) => {
            let configs: Vec<Value> = speakers.iter().map(|(alias, id)| json!({"speakerAlias": alias, "speakerId": id})).collect();
            voice.insert("multiSpeakerVoiceConfig".into(), json!({"speakerVoiceConfigs": configs}));
        }
        None => {
            voice.insert("name".into(), gemini_voice(p, "voice")?.into());
        }
    }
    let body = json!({"input": input, "voice": voice, "audioConfig": {"audioEncoding": "MP3"}});
    Ok(Prepared { body, characters: None })
}

fn prompt(job: &ProviderJob) -> Result<&str, ProviderError> {
    let mut texts = job.inputs.iter().filter_map(|i| match i {
        JobInput::Text { role, text } if role == "prompt" => Some(text.as_str()),
        _ => None,
    });
    let text = texts.next().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| invalid("Google TTS needs a text prompt"))?;
    if texts.next().is_some() {
        return Err(invalid("Google TTS takes one text prompt"));
    }
    Ok(text)
}

/// The classic voice: the override when given, else the chosen voice. It must belong to the
/// model's family, so a run is never billed at another family's price than the one quoted.
/// Returns the name and its language (`en-US` of `en-US-Neural2-F`).
fn classic_voice<'a>(p: &'a Map<String, Value>, family: &Family) -> Result<(&'a str, &'a str), ProviderError> {
    let name = match text(p, "voice_name")? {
        Some(name) => name,
        None => text(p, "voice")?.ok_or_else(|| invalid("Google TTS needs a voice"))?,
    };
    let shaped = name.len() <= VOICE_BYTES && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let mut parts = name.splitn(3, '-');
    let (Some(lang), Some(region), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid("Google TTS voice names look like en-US-Neural2-F"));
    };
    let variant = rest.strip_prefix(family.segment).and_then(|v| v.strip_prefix('-'));
    let lang_ok = (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase());
    let region_ok = (2..=3).contains(&region.len()) && region.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if !shaped || !lang_ok || !region_ok {
        return Err(invalid("Google TTS voice names look like en-US-Neural2-F"));
    }
    if !variant.is_some_and(|v| !v.is_empty()) {
        return Err(invalid(format!("this model speaks with {} voices only, like en-US-{}-…; pick the model for that voice", family.segment, family.segment)));
    }
    let language = name.get(..lang.len() + 1 + region.len()).ok_or_else(|| invalid("Google TTS voice names look like en-US-Neural2-F"))?;
    Ok((name, language))
}
fn gemini_voice(p: &Map<String, Value>, param: &str) -> Result<&'static str, ProviderError> {
    let name = text(p, param)?.unwrap_or("Kore");
    GEMINI_VOICES.iter().find(|v| v.eq_ignore_ascii_case(name)).copied().ok_or_else(|| invalid(format!("Gemini-TTS has no voice named {name:.32}")))
}
fn language(p: &Map<String, Value>) -> Result<&str, ProviderError> {
    let code = text(p, "language_code")?.unwrap_or("en-US");
    let ok = code.len() <= LANGUAGE_BYTES
        && code.split('-').all(|s| (2..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        && code.bytes().next().is_some_and(|b| b.is_ascii_alphabetic());
    if !ok {
        return Err(invalid("Gemini-TTS language codes are BCP-47, like en-US"));
    }
    Ok(code)
}
/// Both speakers when the dialogue names them: `(alias, voice)` each. Lines of the text then
/// start with an alias and a colon (`Sam: Hi!`).
fn speakers(p: &Map<String, Value>) -> Result<Option<[(&str, &str); 2]>, ProviderError> {
    let (first, second) = (text(p, "speaker_1")?, text(p, "speaker_2")?);
    let (first, second) = match (first, second) {
        (None, None) => return Ok(None),
        (Some(a), Some(b)) => (a, b),
        _ => return Err(invalid("a Gemini-TTS dialogue names two speakers; fill in both names or neither")),
    };
    for alias in [first, second] {
        if alias.len() > ALIAS_BYTES || !alias.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(invalid(format!("Gemini-TTS speaker names are letters and digits only, at most {ALIAS_BYTES}")));
        }
    }
    if first == second {
        return Err(invalid("the two Gemini-TTS speakers need different names"));
    }
    Ok(Some([(first, gemini_voice(p, "speaker_1_voice")?), (second, gemini_voice(p, "speaker_2_voice")?)]))
}

/// A non-empty, trimmed text parameter; empty means unset.
fn text<'a>(p: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>, ProviderError> {
    match p.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim()).filter(|s| !s.is_empty())),
        Some(_) => Err(invalid(format!("Google TTS parameter {name} must be text"))),
    }
}
fn number(p: &Map<String, Value>, name: &str, min: f64, max: f64, default: f64) -> Result<f64, ProviderError> {
    let Some(v) = p.get(name) else { return Ok(default) };
    v.as_f64()
        .filter(|n| n.is_finite() && (min..=max).contains(n))
        .ok_or_else(|| invalid(format!("Google TTS parameter {name} must be a number from {min} to {max}")))
}
