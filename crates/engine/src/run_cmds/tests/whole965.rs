//! `run.changed` and `run.workflow` (#965): the whole canvas, quoted first, started only through
//! `run.start` with the quote's mode, token and totals.
use super::*;

fn steps(quote: &Value) -> Vec<Value> {
    quote["steps"].as_array().unwrap().iter().map(|step| step["node"].clone()).collect()
}

/// The fixture without its unpriced generator 3, so what is left can be accepted in full.
fn priced_session() -> (Session, Started) {
    let (mut s, started) = session(Budgets::default(), true);
    s.dispatch("node.remove", &json!({"canvas": C, "id": id(3)})).unwrap();
    (s, started)
}

#[test]
fn run_changed_quotes_without_starting_and_a_confirmed_run_starts_only_what_it_quoted() {
    let (mut s, started) = priced_session();
    let revision = s.revision();
    let quote = s.dispatch("run.changed", &json!({"canvas": C})).unwrap();
    assert_eq!(s.revision(), revision, "a quote changes nothing");
    assert!(started.lock().unwrap().is_empty(), "a quote starts nothing");
    assert_eq!(quote["mode"], "pending");
    assert_eq!(steps(&quote), vec![json!(id(2))], "generator 2 has no result; text 1 is a source");
    assert_eq!(quote["totals"]["USD"]["micros"], 120_000);
    // The same quote as run.quote in that mode, so its token starts it.
    assert_eq!(price(&s, &json!({"canvas": C, "mode": "pending"})).unwrap().token, quote["token"]);
    let accept = json!({"token": quote["token"], "amounts": {"USD": 120_000}});
    assert_eq!(s.dispatch("run.start", &json!({"canvas": C, "mode": "pending", "accept": accept})).unwrap(), json!({"run": "accepted"}));
    let requests = std::mem::take(&mut *started.lock().unwrap());
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0].run.mode, ModeParam::Pending));
    assert_eq!(requests[0].quote().steps.iter().map(|step| step.node).collect::<Vec<_>>(), vec![id(2)]);
    // A token from another mode doesn't start this one.
    let selected = s.dispatch("run.quote", &request(&[2])).unwrap();
    let accept = json!({"token": selected["token"], "amounts": {"USD": 120_000}});
    assert!(s.dispatch("run.start", &json!({"canvas": C, "mode": "pending", "accept": accept})).is_err());
}

#[test]
fn run_workflow_quotes_every_generator_and_starts_with_its_own_mode() {
    let (mut s, started) = priced_session();
    let quote = s.dispatch("run.workflow", &json!({"canvas": C})).unwrap();
    assert_eq!(quote["mode"], "workflow");
    assert_eq!(steps(&quote), vec![json!(id(2))]);
    assert!(started.lock().unwrap().is_empty());
    let accept = json!({"token": quote["token"], "amounts": {"USD": 120_000}});
    s.dispatch("run.start", &json!({"canvas": C, "mode": "workflow", "accept": accept})).unwrap();
    let requests = std::mem::take(&mut *started.lock().unwrap());
    assert!(matches!(requests.first().map(|r| &r.run.mode), Some(ModeParam::Workflow)));
}

#[test]
fn nothing_to_run_is_an_empty_quote_and_bad_params_are_errors() {
    let (mut s, _) = priced_session();
    s.dispatch("node.remove", &json!({"canvas": C, "id": id(2)})).unwrap();
    for id in ["run.changed", "run.workflow"] {
        let quote = s.dispatch(id, &json!({"canvas": C})).unwrap();
        assert!(steps(&quote).is_empty(), "{id}: {quote}");
        assert_eq!(quote["reused"], json!([]));
        assert_eq!(quote["allowed"], true);
    }
    for bad in [json!({}), json!({"canvas": C, "mode": "selected"}), json!({"canvas": "not-a-canvas"}), json!({"canvas": CanvasId::from_bytes([9; 16])})] {
        for id in ["run.changed", "run.workflow"] {
            assert!(s.dispatch(id, &bad).is_err(), "{id} {bad}");
            assert!(s.enabled(id, &bad).is_err(), "{id} {bad}");
        }
    }
    assert!(s.enabled("run.changed", &json!({"canvas": C})).is_ok());
}
