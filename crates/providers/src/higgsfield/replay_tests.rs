//! A request staged before a restart is sent again byte for byte (#664).
use super::*;
use crate::{CancelSignal, Clock, RunLimits, RunUpdate, StagedSubmit, Stager};

struct Never;
impl CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}
struct Instant;
impl Clock for Instant {
    fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}
fn done() -> Vec<Result<HttpResponse, ProviderError>> {
    vec![ok(json!({"request_id": REQUEST, "status": "completed", "video": {"url": "https://cdn.example.com/clip.mp4"}})), file(&mp4())]
}
fn submits(t: &Transport) -> Vec<HttpRequest> {
    t.requests().into_iter().filter(|r| r.method == HttpMethod::Post && r.url.ends_with("/image-to-video")).collect()
}
/// Runs `job` keeping what it stages; returns what was staged and each update.
fn staged_run(p: &Higgsfield, job: &ProviderJob, save: Result<(), String>) -> (Vec<StagedSubmit>, Result<(), crate::RunFailure>) {
    let kept: Arc<Mutex<Vec<StagedSubmit>>> = Arc::default();
    let into = Arc::clone(&kept);
    let stager: Arc<Stager> = Arc::new(move |staged: &StagedSubmit| {
        into.lock().unwrap().push(staged.clone());
        save.clone()
    });
    let result = block_on(crate::run_to_completion_staged(p, job, Some(stager), &Never, &Instant, RunLimits::default(), |_| {}));
    (kept.lock().unwrap().clone(), result.map(|_| ()))
}
/// What a restored job carries: only its model and key.
fn restored() -> ProviderJob {
    ProviderJob { model: KLING_I2V.into(), params: Default::default(), inputs: vec![], idempotency_key: "fixture-operation".into() }
}

#[test]
fn the_body_is_staged_before_it_is_sent_and_a_restart_sends_the_same_bytes() {
    let t = Transport::new(vec![grant(), empty(200), Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let (staged, result) = staged_run(&adapter(&t), &kling(), Ok(()));
    assert!(result.unwrap_err().uncertain);
    let [staged] = staged.try_into().unwrap();
    let [sent] = submits(&t).try_into().unwrap();
    assert_eq!(staged.body.as_bytes(), sent.body.as_slice());
    assert_eq!(staged.data, json!({"v": 1, "model": KLING_I2V, "count": 1}));
    assert!(!staged.body.contains(SECRET) && !staged.body.contains(ID), "no credential is staged");
    // After a restart a fresh adapter, which never staged it, sends the saved bytes again.
    let t2 = Transport::new(vec![receipt()]);
    t2.push(done());
    let mut updates = Vec::new();
    let result = block_on(crate::replay(&adapter(&t2), &restored(), &staged, &Never, &Instant, RunLimits::default(), |u| updates.push(u)));
    result.unwrap();
    assert!(updates.contains(&RunUpdate::Submitted(queued(KLING_I2V, 1))), "{updates:?}");
    let [again] = submits(&t2).try_into().unwrap();
    assert_eq!(again.body, sent.body, "byte for byte");
    assert_eq!(again.url, sent.url);
    assert_eq!(header(&again, "Idempotency-Key"), header(&sent, "Idempotency-Key"));
    assert!(!t2.requests().iter().any(|r| r.url.ends_with("/files/generate-upload-url") || r.method == HttpMethod::Put), "nothing is uploaded again");
}

#[test]
fn a_request_that_cant_be_saved_is_never_sent() {
    let t = Transport::new(vec![grant(), empty(200)]);
    let (_, result) = staged_run(&adapter(&t), &kling(), Err("the disk is full".into()));
    let failure = result.unwrap_err();
    assert!(failure.error.message.contains("the disk is full"), "{failure:?}");
    assert!(submits(&t).is_empty());
}

#[test]
fn an_expired_staging_link_is_reported_not_staged_again() {
    let t = Transport::new(vec![grant(), empty(200), Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let (staged, _) = staged_run(&adapter(&t), &kling(), Ok(()));
    let [staged] = staged.try_into().unwrap();
    let t2 = Transport::new(vec![status(422, json!({"detail": "image_url could not be downloaded"}))]);
    let failure = block_on(crate::replay(&adapter(&t2), &restored(), &staged, &Never, &Instant, RunLimits::default(), |_| {})).unwrap_err();
    assert!(!failure.is_transient(), "{failure:?}");
    assert!(failure.error.message.contains("could not be downloaded") && failure.error.message.contains("may have expired"), "{failure:?}");
    no_secret(&failure.error);
    let [only] = t2.requests().try_into().unwrap();
    assert!(only.url.ends_with("/image-to-video"), "sent once, never uploaded afresh");
}

#[test]
fn a_damaged_staged_request_is_never_sent() {
    let good = StagedSubmit { body: json!({"prompt": "x"}).to_string(), data: json!({"v": 1, "model": KLING_I2V, "count": 1}) };
    let damaged = [
        StagedSubmit { body: "not json".into(), ..good.clone() },
        StagedSubmit { data: json!({"v": 1, "model": SOUL, "count": 1}), ..good.clone() },
        StagedSubmit { data: json!({"v": 2, "model": KLING_I2V}), ..good.clone() },
        // Polling only follows one Kling clip: four would be paid for and never found.
        StagedSubmit { data: json!({"v": 1, "model": KLING_I2V, "count": 4}), ..good.clone() },
        StagedSubmit { data: json!({"v": 1, "model": KLING_I2V}), ..good.clone() },
        StagedSubmit { body: format!("\"{}\"", "a".repeat(crate::MAX_STAGED_BODY)), ..good.clone() },
    ];
    for staged in damaged {
        let t = Transport::new(vec![]);
        let failure = block_on(crate::replay(&adapter(&t), &restored(), &staged, &Never, &Instant, RunLimits::default(), |_| {})).unwrap_err();
        assert!(failure.error.message.contains("damaged"), "{failure:?}");
        assert!(t.requests().is_empty());
    }
    let mut unknown = restored();
    unknown.model = "someone/else".into();
    let t = Transport::new(vec![]);
    block_on(crate::replay(&adapter(&t), &unknown, &good, &Never, &Instant, RunLimits::default(), |_| {})).unwrap_err();
    assert!(t.requests().is_empty());
}
