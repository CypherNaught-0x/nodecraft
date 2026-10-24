//! Soul ID training (#680): creation, polling to the end, and a lost reply resolved by listing.
use super::*;
use crate::higgsfield::training;

fn photos(n: usize) -> Vec<JobInput> {
    (0..n).map(|_| JobInput::Media { role: "images".into(), mime: "image/png".into(), bytes: Arc::from(png()) }).collect()
}
fn trainer(name: &str, inputs: Vec<JobInput>) -> ProviderJob {
    job(SOUL_TRAINER, json!({"name": name}), inputs)
}
/// The name the fixture job's Soul ID is sent under.
fn tagged(name: &str) -> String {
    format!("{name}{}", training::tag("fixture-operation"))
}
fn listed(items: Vec<Value>) -> Result<HttpResponse, ProviderError> {
    ok(json!({"items": items, "total": items.len(), "page": 1, "page_size": 50, "total_pages": 1}))
}
fn end() -> Result<HttpResponse, ProviderError> {
    ok(json!({"items": []}))
}
fn created(name: &str) -> Result<HttpResponse, ProviderError> {
    ok(json!({"id": SOUL_ID, "name": name, "status": "not_ready", "created_at": "2026-10-22T10:00:00Z", "thumbnail_url": null, "in_progress_at": null}))
}
fn training_handle() -> JobHandle {
    JobHandle { remote_id: SOUL_ID.into(), data: json!({"v": 1, "model": SOUL_TRAINER, "count": 1}) }
}
fn posts(t: &Transport) -> usize {
    t.requests().iter().filter(|r| r.method == HttpMethod::Post && r.url.ends_with("/v1/custom-references")).count()
}

#[test]
fn a_soul_id_is_trained_from_the_uploaded_photos_under_a_tagged_name() {
    let t = Transport::new(vec![end(), grant(), empty(200), grant(), empty(200), created(&tagged("Mara"))]);
    let Submission::Queued(handle) = block_on(adapter(&t).submit(&trainer(" Mara ", photos(2)))).unwrap() else { panic!("queued") };
    assert_eq!(handle, training_handle());
    let requests = t.requests();
    // The account is looked through first, then the photos go to storage, then the one paid POST.
    assert_eq!(requests[0].url, "https://api.higgsfield.ai/v1/custom-references/list?page=1&page_size=50");
    assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Put).count(), 2);
    let create = requests.last().unwrap();
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, "https://api.higgsfield.ai/v1/custom-references"));
    assert_eq!(header(create, "Authorization"), Some(format!("Key {ID}:{SECRET}").as_str()));
    assert_eq!(header(create, "Idempotency-Key"), None, "Higgsfield documents none for training");
    assert_eq!(
        body(create),
        json!({"name": tagged("Mara"), "model_version": "v2", "input_images": [{"type": "image_url", "image_url": PUBLIC}, {"type": "image_url", "image_url": PUBLIC}]})
    );
    // The tag is the key's, so another run's training gets another one.
    assert!(tagged("Mara").len() <= 100 && tagged("Mara").starts_with("Mara [nc-"));
    assert_ne!(training::tag("fixture-operation"), training::tag("another-operation"));
}

#[test]
fn polling_follows_the_training_to_a_descriptor_naming_the_soul_id() {
    let state = |status: &str| ok(json!({"id": SOUL_ID, "name": tagged("Mara"), "status": status, "fail_reason": null}));
    let t = Transport::new(vec![state("not_ready"), state("queued"), state("in_progress"), state("completed")]);
    let higgsfield = adapter(&t);
    assert!(matches!(block_on(higgsfield.poll(&training_handle())).unwrap(), JobProgress::Queued { .. }));
    assert!(matches!(block_on(higgsfield.poll(&training_handle())).unwrap(), JobProgress::Queued { .. }));
    assert!(matches!(block_on(higgsfield.poll(&training_handle())).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(higgsfield.poll(&training_handle())).unwrap() else { panic!("done") };
    let [Output::Bytes { mime, bytes }] = &done.outputs[..] else { panic!("{:?}", done.outputs) };
    assert_eq!(mime, nodecraft_media::style::MIME);
    let style = nodecraft_media::style::decode(bytes).unwrap();
    assert_eq!((style.provider.as_str(), style.trainer.as_str(), style.family.as_str()), ("higgsfield", SOUL_TRAINER, SOUL_FAMILY));
    assert_eq!(style.weights, format!("https://api.higgsfield.ai/v1/custom-references/{SOUL_ID}"));
    assert!(t.requests().iter().all(|r| r.method == HttpMethod::Get && r.url.ends_with(SOUL_ID)));
    // A state Higgsfield doesn't document keeps the card waiting rather than ending it.
    let t = Transport::new(vec![state("archiving")]);
    assert!(matches!(block_on(adapter(&t).poll(&training_handle())).unwrap(), JobProgress::Running { .. }));
}

#[test]
fn a_failed_training_ends_with_higgsfields_reason() {
    let failed = |reason: Value| ok(json!({"id": SOUL_ID, "name": tagged("Mara"), "status": "failed", "fail_reason": reason}));
    for (reply, says) in [
        (failed(json!("no face found in 3 images")), "couldn't train this Soul ID: no face found in 3 images"),
        (failed(Value::Null), "couldn't train this Soul ID"),
        (failed(json!(format!("bad key {SECRET}"))), "(message withheld)"),
    ] {
        let t = Transport::new(vec![reply]);
        let JobProgress::Failed(e) = block_on(adapter(&t).poll(&training_handle())).unwrap() else { panic!("failed") };
        assert_eq!(e.kind, ErrorKind::JobFailed);
        assert!(e.message.contains(says), "{says}: {e:?}");
        no_secret(&e);
    }
    // A reply about another Soul ID, or one trained for another SOUL, is refused.
    for reply in [
        ok(json!({"id": "11111111-2222-4333-8444-555555555555", "status": "completed"})),
        ok(json!({"id": SOUL_ID, "model_version": "v1", "status": "completed"})),
        ok(json!({"id": SOUL_ID})),
    ] {
        let t = Transport::new(vec![reply]);
        assert_eq!(block_on(adapter(&t).poll(&training_handle())).unwrap_err().kind, ErrorKind::Malformed);
    }
}

#[test]
fn a_lost_reply_is_resolved_by_listing_and_never_sent_again() {
    let job = trainer("Mara", photos(1));
    let lost = || Err(ProviderError::new(ErrorKind::Network, "connection reset"));
    let t = Transport::new(vec![end(), grant(), empty(200), lost()]);
    let higgsfield = adapter(&t);
    let e = block_on(higgsfield.submit(&job)).unwrap_err();
    assert!(e.message.contains("may have been accepted and billed"), "{e:?}");
    assert!(higgsfield.retry_ambiguous_submit());
    // The retry finds the Soul ID by its tag, among others with the same name, and pays nothing.
    let others = vec![
        json!({"id": "11111111-2222-4333-8444-555555555555", "name": "Mara", "status": "completed"}),
        json!({"id": "22222222-2222-4333-8444-555555555555", "name": format!("Mara{}", training::tag("an-older-run")), "status": "completed"}),
        json!({"id": SOUL_ID, "name": tagged("Mara"), "status": "queued"}),
    ];
    t.push(vec![listed(others.clone()), end()]);
    assert_eq!(block_on(higgsfield.submit(&job)).unwrap(), Submission::Queued(training_handle()));
    assert_eq!(posts(&t), 1);
    // Not listed yet: the retry fails without sending it again, and says why.
    let t = Transport::new(vec![end(), grant(), empty(200), lost(), end()]);
    let higgsfield = adapter(&t);
    block_on(higgsfield.submit(&job)).unwrap_err();
    let e = block_on(higgsfield.submit(&job)).unwrap_err();
    assert!(e.message.contains("wasn't sent again") && !e.is_transient(), "{e:?}");
    assert_eq!(posts(&t), 1);
    // Once it shows, it is picked up, even by a new process that never sent it.
    t.push(vec![listed(others), end()]);
    assert_eq!(block_on(adapter(&t).submit(&job)).unwrap(), Submission::Queued(training_handle()));
    assert_eq!(posts(&t), 1);
}

#[test]
fn a_refused_training_may_be_sent_again() {
    let job = trainer("Mara", photos(1));
    let t = Transport::new(vec![end(), grant(), empty(200), status(403, json!({"detail": "Not enough credits"}))]);
    let higgsfield = adapter(&t);
    let e = block_on(higgsfield.submit(&job)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    t.push(vec![end(), grant(), empty(200), created(&tagged("Mara"))]);
    block_on(higgsfield.submit(&job)).unwrap();
    assert_eq!(posts(&t), 2);
}

#[test]
fn an_unreadable_creation_reply_is_retried_by_listing_not_by_training_again() {
    let job = trainer("Mara", photos(1));
    // Higgsfield may tidy the name it echoes: the tag is what identifies the training.
    let tidied = ok(json!({"id": SOUL_ID, "name": tagged("Mara").replace(" [", "  ["), "status": "not_ready"}));
    let t = Transport::new(vec![end(), grant(), empty(200), tidied]);
    assert_eq!(block_on(adapter(&t).submit(&job)).unwrap(), Submission::Queued(training_handle()));
    // A reply that doesn't read after a paid POST is a server fault, retried under the same key,
    // and the retry adopts the listed training.
    for reply in [ok(json!({"status": "not_ready"})), ok(json!({"id": SOUL_ID, "name": "Someone else"})), empty(200)] {
        let t = Transport::new(vec![end(), grant(), empty(200), reply]);
        let higgsfield = adapter(&t);
        let e = block_on(higgsfield.submit(&job)).unwrap_err();
        assert!(e.is_transient(), "{e:?}");
        t.push(vec![listed(vec![json!({"id": SOUL_ID, "name": tagged("Mara"), "status": "queued"})]), end()]);
        assert_eq!(block_on(higgsfield.submit(&job)).unwrap(), Submission::Queued(training_handle()));
        assert_eq!(posts(&t), 1);
    }
}

#[test]
fn a_creation_that_may_have_been_made_is_never_taken_for_a_refusal() {
    let job = trainer("Mara", photos(1));
    let t = Transport::new(vec![end(), grant(), empty(200), status(503, json!({"detail": "Service unavailable"}))]);
    let higgsfield = adapter(&t);
    block_on(higgsfield.submit(&job)).unwrap_err();
    // Not listed: the training isn't sent a second time.
    t.push(vec![end()]);
    let e = block_on(higgsfield.submit(&job)).unwrap_err();
    assert!(e.message.contains("wasn't sent again"), "{e:?}");
    assert_eq!(posts(&t), 1);
}

#[test]
fn an_account_that_cant_be_read_in_full_trains_nothing() {
    let job = trainer("Mara", photos(1));
    let two = vec![json!({"id": SOUL_ID, "name": tagged("Mara")}), json!({"id": "11111111-2222-4333-8444-555555555555", "name": tagged("Mara")})];
    let full = || ok(Value::Array(vec![json!({}); 50]));
    for replies in [vec![listed(two), end()], (0..21).map(|_| full()).collect(), vec![ok(json!({"data": []}))], vec![status(401, json!({"detail": "bad key"}))]]
    {
        let t = Transport::new(replies);
        let e = block_on(adapter(&t).submit(&job)).unwrap_err();
        assert_eq!(posts(&t), 0, "{e:?}");
        no_secret(&e);
    }
}

#[test]
fn training_jobs_are_checked_before_anything_is_sent() {
    let cases = [
        trainer("", photos(1)),
        trainer("   ", photos(1)),
        trainer(&"x".repeat(training::NAME_CHARS + 1), photos(1)),
        trainer("Line\nbreak", photos(1)),
        trainer("Mara", vec![]),
        trainer("Mara", photos(training::MAX_IMAGES + 1)),
        trainer("Mara", vec![prompt("a face")]),
        trainer("Mara", vec![image()]),
        trainer("Mara", vec![JobInput::Media { role: "images".into(), mime: "image/gif".into(), bytes: Arc::from(png()) }]),
        trainer("Mara", vec![JobInput::Media { role: "images".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) }]),
        job(SOUL_TRAINER, json!({}), photos(1)),
        job(SOUL_TRAINER, json!({"name": "Mara", "steps": 3}), photos(1)),
        job(SOUL_TRAINER, json!({"name": 7}), photos(1)),
    ];
    for job in cases {
        let t = Transport::new(vec![]);
        let e = block_on(adapter(&t).submit(&job)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{e:?}");
        assert!(t.requests().is_empty());
    }
    // The longest name fits Higgsfield's 100 characters with its tag, and 100 photos are taken.
    let name = "é".repeat(training::NAME_CHARS);
    assert_eq!(training::prepare(&trainer(&name, photos(training::MAX_IMAGES)), Limits::HARD).unwrap().name.chars().count(), 100);
    // There is no estimate to ask for, and a training can't be stopped.
    let t = Transport::new(vec![]);
    assert!(block_on(adapter(&t).estimate(&trainer("Mara", photos(1)))).is_err());
    assert!(block_on(adapter(&t).cancel(&training_handle())).unwrap_err().message.contains("can't stop"));
    assert!(t.requests().is_empty());
}

#[test]
fn the_picker_shows_a_trained_soul_id_by_the_name_it_was_given() {
    assert_eq!(training::shown(&tagged("Mara")), "Mara");
    for name in ["Mara", "Mara [nc-]", "Mara [nc-zzzzzzzzzz]", "Mara [nc-0123456789] later"] {
        assert_eq!(training::shown(name), name);
    }
    let t = Transport::new(vec![listed(vec![json!({"id": SOUL_ID, "name": tagged("Mara"), "model_version": "v2", "status": "completed"})]), end()]);
    let ids = block_on(adapter(&t).soul_ids()).unwrap();
    assert_eq!(ids.ids, [SoulId { id: SOUL_ID.into(), name: "Mara".into() }]);
}

#[test]
fn the_catalog_trains_what_the_adapter_sends() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    let entry = catalog.get("higgsfield.soul-id-trainer").unwrap();
    assert_eq!((entry.provider.as_str(), entry.provider_model.as_str()), ("higgsfield", SOUL_TRAINER));
    assert_eq!(entry.style_family.as_deref(), Some(SOUL_FAMILY));
    assert!(matches!(entry.pricing, nodecraft_catalog::Pricing::PerCall { .. }), "priced before it is sent");
    training::prepare(&trainer("Mara", photos(1)), Limits::HARD).unwrap();
}
