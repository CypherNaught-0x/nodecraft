use super::*;
use crate::{HttpResponse, Output, Usage};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reply {
    results: Results,
}
#[derive(Deserialize)]
struct Results {
    channels: Vec<Channel>,
}
#[derive(Deserialize)]
struct Channel {
    alternatives: Vec<Alternative>,
}
#[derive(Deserialize)]
struct Alternative {
    transcript: String,
}

pub(super) fn parse(reply: HttpResponse, key: &Secret) -> Result<Completed, ProviderError> {
    if reply.body.len() > MAX_RESPONSE_BYTES {
        return Err(malformed());
    }
    if reply.status != 200 {
        // Never publish provider text, which may contain credentials or private transcript data.
        let kind = if (200..400).contains(&reply.status) { ErrorKind::Malformed } else { ProviderError::from_status(reply.status, b"").kind };
        let mut error = ProviderError::new(kind, format!("Deepgram transcription failed (HTTP {})", reply.status));
        error.retry_after_ms = reply
            .header("retry-after")
            .filter(|v| v.len() <= 16)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .and_then(|n| n.checked_mul(1000))
            .filter(|n| *n <= 3_600_000);
        return Err(error);
    }
    if reply.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(malformed());
    }
    let parsed: Reply = serde_json::from_slice(&reply.body).map_err(|_| malformed())?;
    let [channel] = parsed.results.channels.as_slice() else { return Err(malformed()) };
    let [alternative] = channel.alternatives.as_slice() else { return Err(malformed()) };
    if alternative.transcript.len() > MAX_TRANSCRIPT_BYTES || alternative.transcript.contains(key.expose()) {
        return Err(malformed());
    }
    // Silence legitimately yields empty text. The API does not return a monetary receipt.
    Ok(Completed { outputs: vec![Output::Text(alternative.transcript.clone())], usage: Usage::default() })
}
