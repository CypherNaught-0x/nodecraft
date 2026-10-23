//! Disabled paid smokes: one short Flash v2.5 sentence (about 0.002 USD), and a few streamed v3
//! sentences (#875, about 300 characters). Run them only with explicit human spending
//! authorization and `NODECRAFT_LIVE_ELEVENLABS=1`.
#![cfg(all(feature = "http", feature = "elevenlabs", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    BoxFuture, CancelSignal, Clock, JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, RunLimits, RunUpdate, Submission,
    elevenlabs::{ElevenLabs, FLASH_V2_5, V3},
    run_to_completion,
    secrets::Secret,
};
use std::{sync::Arc, time::Instant};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and NODECRAFT_LIVE_ELEVENLABS=1 required"]
async fn live_elevenlabs() {
    if std::env::var("NODECRAFT_LIVE_ELEVENLABS").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("ELEVENLABS_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = ElevenLabs::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let text = "NodeCraft speaks.";
    let j = ProviderJob {
        model: FLASH_V2_5.into(),
        params: serde_json::json!({"voice": "EXAVITQu4vr4xnSDxMaL"}).as_object().cloned().unwrap(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: text.into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let Submission::Finished(c) = p.submit(&j).await.unwrap() else { panic!("synchronous") };
    let [Output::Bytes { mime, bytes }] = c.outputs.as_slice() else { panic!("one MP3") };
    assert_eq!(mime, "audio/mpeg");
    assert!(bytes.len() > 1000, "{} bytes", bytes.len());
    assert_eq!(c.usage.billable_units.map(|u| u.millionths()), Some(text.chars().count() as u64 * 1_000_000));
}

struct Never;
impl CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}
struct RealClock(Instant);
impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))
    }
}

/// `/stream` with eleven_v3 and mp3_44100_128 (#875): previews arrive, grow, are each a prefix
/// of the result, and none is the whole reply.
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and NODECRAFT_LIVE_ELEVENLABS=1 required"]
async fn live_elevenlabs_v3_stream_previews() {
    if std::env::var("NODECRAFT_LIVE_ELEVENLABS").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("ELEVENLABS_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = ElevenLabs::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    // Long enough for more than 64 KiB of MP3 at 128 kb/s (about four seconds).
    let text = "NodeCraft speaks while the sound arrives. Each preview is the speech so far, and the card plays it at once. \
                When the whole reply is in, the card switches to the finished sound and keeps playing from where it was.";
    let j = ProviderJob {
        model: V3.into(),
        params: serde_json::json!({"voice": "EXAVITQu4vr4xnSDxMaL"}).as_object().cloned().unwrap(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: text.into() }],
        idempotency_key: "explicit-live-stream-no-server-dedup".into(),
    };
    let mut previews: Vec<Arc<[u8]>> = Vec::new();
    let limits = RunLimits { stream_previews: true, deadline_ms: 120_000, ..Default::default() };
    let c = run_to_completion(&p, &j, &Never, &RealClock(Instant::now()), limits, |update| {
        if let RunUpdate::Progress(JobProgress::Running { partial: Some(Output::Bytes { mime, bytes }), .. }) = update {
            assert_eq!(mime, "audio/mpeg");
            previews.push(bytes);
        }
    })
    .await
    .unwrap();
    let [Output::Bytes { mime, bytes }] = c.outputs.as_slice() else { panic!("one MP3") };
    assert_eq!(mime, "audio/mpeg");
    assert!(!previews.is_empty(), "a {} byte reply streamed no preview", bytes.len());
    assert!(previews.windows(2).all(|w| w[0].len() < w[1].len()), "previews grow");
    assert!(previews.iter().all(|p| p.len() < bytes.len() && bytes.starts_with(p)), "each preview is a prefix, never the whole reply");
}
