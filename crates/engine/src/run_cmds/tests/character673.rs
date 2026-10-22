use super::*;

fn character_session(limit: u64) -> (Session, Started) {
    let (mut session, started) = session(Budgets { per_call: Some(usd(limit)), ..Default::default() }, true);
    let catalog = CATALOG.replace(
        "pricing = { type = \"per_unit\", unit = \"image\", price = \"0.04 USD\", count = \"num_images\" }",
        "pricing = { type = \"per_unit\", unit = \"thousand_characters\", price = \"0.04 USD\" }",
    );
    let _ = session.swap_resource(Some(RunCatalog::new(Catalog::from_toml(&catalog).unwrap()).unwrap()));
    (session, started)
}

#[test]
fn character_quote_reaches_run_admission_and_the_queued_reservation() {
    let (mut session, started) = character_session(200);
    // The wired source is "a fox": five characters at 0.04 USD / 1,000.
    let quote = session.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(quote["totals"]["USD"]["micros"], 200);
    assert_eq!(quote["complete"], true);
    assert_eq!(quote["allowed"], true);
    let start = with_accept(&[2], json!({"token": quote["token"], "amounts": {"USD": 200}}));
    session.dispatch("run.start", &start).unwrap();
    let prepared = started.lock().unwrap().pop().unwrap();
    let (run, progress) = session.authorize_prepared_run(prepared, run_options()).unwrap();
    let queued = run.queued_job(progress.jobs[0].id, "characters673").unwrap();
    assert_eq!(queued.price.unwrap().estimate, Some(usd(200)));
    assert!(matches!(queued.job.inputs.as_slice(), [nodecraft_providers::JobInput::Text { text, .. }] if text == "a fox"));

    let (mut too_low, started) = character_session(199);
    let quote = too_low.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(quote["allowed"], false);
    let start = with_accept(&[2], json!({"token": quote["token"], "amounts": {"USD": 200}}));
    assert!(too_low.dispatch("run.start", &start).is_err());
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn editing_the_wired_text_invalidates_the_character_quote() {
    let (mut session, _) = character_session(200);
    let old = session.dispatch("run.quote", &request(&[2])).unwrap();
    let start = with_accept(&[2], json!({"token": old["token"], "amounts": {"USD": 200}}));
    session.dispatch("node.param_set", &json!({"canvas": C, "id": id(1), "name": "text", "value": {"type": "text", "value": "雪é🙂"}})).unwrap();
    assert!(session.dispatch("run.start", &start).is_err());
    let new = session.dispatch("run.quote", &request(&[2])).unwrap();
    assert_eq!(new["totals"]["USD"]["micros"], 120);
    assert_ne!(old["token"], new["token"]);
}
