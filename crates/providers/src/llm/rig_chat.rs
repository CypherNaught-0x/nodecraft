//! What both chat dialects share on `rig-core` (#1122): sending a request through a wire bound to
//! [`RigHttp`], whole or streamed, and reading the crate's errors in NodeCraft's terms.

use futures::StreamExt as _;
use rig_core::Model;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::operation::Completion;
use rig_core::streaming::{Item, StreamEvent};
use rig_core::wire::{Encoded, Wire, WireFrame};
use serde_json::Value;

use crate::rig_http::{Exchange, RigHttp};
use crate::{ErrorKind, ProviderError};

/// How a dialect reads its errors.
pub(super) struct Rules {
    /// An error status and its body.
    pub status: fn(u16, &[u8]) -> ProviderError,
    /// The `error` object of an error event inside a 200 reply.
    pub event: fn(&Value) -> ProviderError,
    /// What a whole reply that doesn't end the turn is called.
    pub unended: &'static str,
}

/// The crate's error in NodeCraft's terms, from what the transport saw first: its own failure as
/// it raised it, an error status by the dialect's rules, then the crate's reading of the reply.
pub(super) fn failure(error: &rig_core::ProviderError, exchange: Exchange, streamed: bool, rules: &Rules) -> ProviderError {
    match exchange {
        Exchange::Failed(e) => return e,
        Exchange::Answered { status, headers, body } if !(200..300).contains(&status) => {
            let mut e = (rules.status)(status, &body);
            e.retry_after_ms = super::retry_after(headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("retry-after")).map(|(_, v)| v.as_str()));
            return e;
        }
        _ => {}
    }
    let malformed = |what: String| ProviderError::new(ErrorKind::Malformed, what);
    // An error event inside a 200 reply.
    if let Some(body) = error.provider_response_body() {
        return match serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("error").cloned()) {
            Some(err) => (rules.event)(&err),
            None => malformed(format!("stream event: {}", crate::error::excerpt(body.as_bytes()))),
        };
    }
    let message = crate::error::excerpt(error.to_string().as_bytes());
    match error {
        rig_core::ProviderError::Truncated if streamed => ProviderError::new(ErrorKind::Network, "the reply stream ended early"),
        rig_core::ProviderError::Truncated => malformed(rules.unended.to_owned()),
        rig_core::ProviderError::Stalled(_) => ProviderError::new(ErrorKind::Timeout, "the reply stream stalled"),
        // Refused before anything was sent.
        rig_core::ProviderError::Request(_) | rig_core::ProviderError::UnsupportedOption(_) => ProviderError::new(ErrorKind::UnsupportedInput, message),
        _ => malformed(format!("chat reply: {message}")),
    }
}

/// A temperature through its decimal text, so 0.7 goes as 0.7, not as the nearest f32.
pub(super) fn temperature(t: Option<f32>) -> Option<f64> {
    t.and_then(|t| t.to_string().parse::<f64>().ok())
}

/// Sends `request` on `model`, whose transport is `transport`: streamed when `on_text` takes the
/// pieces of text as they arrive. Once the reply is whole, a send the crate stopped reading at the
/// last event runs to its end, so the reply's head is read and its delivery closes as the
/// transport finished it; however this ends otherwise (an error, a cancel), such a send is dropped.
pub(super) async fn send<W>(
    model: Model<W, RigHttp>,
    transport: &RigHttp,
    request: CompletionRequest,
    on_text: Option<&mut (dyn FnMut(&str) + Send)>,
    rules: &Rules,
) -> Result<CompletionResponse, ProviderError>
where
    W: Wire<Op = Completion, Payload = Encoded, Frame = WireFrame>,
{
    // Declared before the stream, so it is dropped after the stream that may leave a send.
    let _discard = Discard(transport.clone());
    let streamed = on_text.is_some();
    let response = match on_text {
        None => model.call(request).await,
        Some(on_text) => {
            let mut stream = model.stream(request).map_err(|e| failure(&e, transport.exchange(), streamed, rules))?;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(Item::Event(StreamEvent::Text { text, .. })) => on_text(&text),
                    Ok(_) => {}
                    Err(e) => return Err(failure(&e, transport.exchange(), streamed, rules)),
                }
            }
            stream.finish().await
        }
    };
    let response = response.map_err(|e| failure(&e, transport.exchange(), streamed, rules))?;
    transport.finish().await?;
    Ok(response)
}

/// Drops the transport's unfinished send when [`send`] ends, however it ends.
struct Discard(RigHttp);

impl Drop for Discard {
    fn drop(&mut self) {
        self.0.discard();
    }
}
