//! ElevenLabs speech, sound effects, music and voice changer: one synchronous request each, the
//! MP3 in the reply body. No automatic paid replay. See docs/providers.md.
mod request;
mod response;
#[cfg(test)]
mod tests;
use crate::secrets::Secret;
use crate::{BoxFuture, Completed, ErrorKind, Http, JobHandle, JobProgress, Provider, ProviderError, ProviderJob, Submission, SubmitContext};
use std::sync::Arc;

pub const V3: &str = "eleven_v3";
pub const MULTILINGUAL_V2: &str = "eleven_multilingual_v2";
pub const FLASH_V2_5: &str = "eleven_flash_v2_5";
pub const SOUND_EFFECTS: &str = "eleven_text_to_sound_v2";
pub const MUSIC: &str = "music_v1";
pub const VOICE_CHANGER: &str = "eleven_multilingual_sts_v2";
const API: &str = "https://api.elevenlabs.io";
/// Every endpoint is asked for the same output: MP3, 44.1 kHz, 128 kbps.
const OUTPUT_FORMAT: &str = "mp3_44100_128";
const MIB: usize = 1024 * 1024;
/// The id of the account's voice list (#670), as parameter contracts name it.
pub const VOICES: &str = "elevenlabs.voices";
/// Voices asked for a page, ElevenLabs's most.
const VOICE_PAGE: usize = 100;
/// Pages read before giving up: 5000 voices.
const VOICE_PAGES: usize = 50;
/// The largest page read; a page of 100 voices with their settings and labels is far smaller.
const VOICE_PAGE_BYTES: usize = 8 * MIB;

/// Local operation limits, not service maxima. Every field can only be lowered by `with_limits`.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The largest reply read: ten minutes of 128 kbps MP3 is about 9.6 MB.
    pub output_bytes: usize,
    /// The largest recording the voice changer uploads.
    pub input_bytes: usize,
    /// The longest sound-effect or music description, in characters.
    pub description_chars: usize,
}
impl Limits {
    pub const HARD: Self = Self { output_bytes: 32 * MIB, input_bytes: 50 * MIB, description_chars: 5000 };
    fn validate(self) -> Result<Self, ProviderError> {
        let h = Self::HARD;
        if self.output_bytes > h.output_bytes || self.input_bytes > h.input_bytes || self.description_chars > h.description_chars {
            return Err(invalid("ElevenLabs limits may only be lowered"));
        }
        Ok(self)
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self::HARD
    }
}
fn invalid(s: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, s)
}
fn malformed(s: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, s)
}

/// A voice the account can speak with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voice {
    pub id: String,
    pub name: String,
}

/// The account's voices in ElevenLabs's order, premade ones included, and how many entries
/// didn't read as a voice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Voices {
    pub voices: Vec<Voice>,
    pub skipped: usize,
}

/// Fixed-origin synchronous adapter. Constructing a provider never reads credentials or calls HTTP.
pub struct ElevenLabs {
    key: Secret,
    http: Arc<dyn Http>,
    limits: Limits,
}
impl ElevenLabs {
    pub fn new(key: Secret, http: Arc<dyn Http>) -> Self {
        Self { key, http, limits: Limits::HARD }
    }
    pub fn with_limits(key: Secret, http: Arc<dyn Http>, limits: Limits) -> Result<Self, ProviderError> {
        Ok(Self { key, http, limits: limits.validate()? })
    }
    /// The voices the account has (#670), every page of `GET /v2/voices`. Free, and sends nothing
    /// but the key.
    pub async fn voices(&self) -> Result<Voices, ProviderError> {
        let mut out = Voices::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut token: Option<String> = None;
        let mut asked = std::collections::HashSet::new();
        for _ in 0..VOICE_PAGES {
            let mut url = format!("{API}/v2/voices?page_size={VOICE_PAGE}&include_total_count=false");
            if let Some(token) = &token {
                url.push_str("&next_page_token=");
                url.push_str(&response::query_escaped(token));
            }
            let list = crate::HttpRequest::get(url).timeout_ms(30_000).secret_header("xi-api-key", self.key.expose()).max_response_bytes(VOICE_PAGE_BYTES);
            let reply =
                self.http.send_no_redirect(list).await.map_err(|e| ProviderError { message: "ElevenLabs's voice list couldn't be reached".into(), ..e })?;
            let page = response::voice_page(reply, &self.key)?;
            out.skipped = out.skipped.saturating_add(page.skipped);
            for voice in page.voices {
                if seen.insert(voice.id.clone()) {
                    out.voices.push(voice);
                }
            }
            match page.next {
                // A page that points back at one already read would go round for ever.
                Some(next) if !asked.insert(next.clone()) => return Err(malformed("ElevenLabs's voice list pointed back at a page already read")),
                Some(next) => token = Some(next),
                None => return Ok(out),
            }
        }
        Err(malformed("this ElevenLabs account has more voices than NodeCraft lists"))
    }
    async fn perform(&self, job: &ProviderJob, context: Option<&SubmitContext>) -> Result<Completed, ProviderError> {
        let prepared = request::prepare(job, self.limits)?;
        let req = prepared.request.secret_header("xi-api-key", self.key.expose()).max_response_bytes(self.limits.output_bytes);
        // Everything local is done; from here a lost reply may hide a billed generation, unless
        // ElevenLabs answers with a documented refusal.
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(req).await.map_err(|e| ProviderError {
            kind: e.kind,
            message: "ElevenLabs transport failed; the audio may have been generated and billed".into(),
            retry_after_ms: e.retry_after_ms.filter(|n| *n <= 3_600_000),
        })?;
        if let Some(context) = context
            && response::refused(&reply)
        {
            context.refused();
        }
        response::parse(reply, self.limits, &self.key, prepared.characters)
    }
}
impl Provider for ElevenLabs {
    fn id(&self) -> &str {
        "elevenlabs"
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move { self.perform(j, None).await.map(Submission::Finished) })
    }
    fn submit_with_delivery<'a>(&'a self, j: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        context.arm();
        Box::pin(async move { self.perform(j, Some(context)).await.map(Submission::Finished) })
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "ElevenLabs synchronous requests have no resumable handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Err(invalid("ElevenLabs cannot cancel a sent synchronous request")) })
    }
}
