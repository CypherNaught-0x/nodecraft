//! Paid runs from automation (#1079): a client's run commands are routed to their providers
//! without the person pressing Generate, and a paid start waits for the person's answer in the
//! window, which these tests give with real pointer events. Nothing here reads the keychain or
//! calls a real provider: the provider is the mock.
use super::automation_runs::{ASKING, ASKING_OTHER, AutomationClient, DECLINED, NEVER, RunGate, SETTING_UP};
use super::runs_tests::{C, MOCK_IMAGE, N, frames_until, idle, image, making, node, scene, scene_in};
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_provider_testkit::mock::{MockModel, MockOutput};
use serde_json::Value as Json;

const CONTROL: AutomationClient = AutomationClient::Control;

fn quote_params() -> Json {
    json!({"canvas": C, "mode": "selected", "nodes": [N]})
}

/// The gate, frame by frame as the control pump asks it, until the providers are set up.
fn routed(h: &mut Harness<'static, NodeCraftApp>, client: AutomationClient, id: &str, params: &Json) -> RunGate {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let gate = h.state_mut().automation_run_gate(client, id, params, false).unwrap();
        if gate != RunGate::Waiting(SETTING_UP) {
            return gate;
        }
        assert!(std::time::Instant::now() < deadline, "the providers were never set up; notice: {:?}", h.state().notice);
        frames_until(h, |_| true);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Quotes the card as a client would, once its provider is set up, and returns `run.start`'s
/// parameters accepting that quote.
fn quoted(h: &mut Harness<'static, NodeCraftApp>) -> Json {
    assert_eq!(routed(h, CONTROL, "run.quote", &quote_params()), RunGate::Ready);
    let quote = h.state_mut().session.dispatch("run.quote", &quote_params()).unwrap();
    assert_eq!(quote["allowed"], true, "{quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], 40_000, "{quote}");
    json!({"canvas": C, "mode": "selected", "nodes": [N], "accept": {"token": quote["token"], "amounts": {"USD": 40_000}}})
}

fn list(h: &mut Harness<'static, NodeCraftApp>) -> Json {
    h.state_mut().session.dispatch("automation.run_consent_list", &json!({})).unwrap()
}

fn decision(h: &mut Harness<'static, NodeCraftApp>, client: &str) -> Json {
    let listed = list(h);
    listed["clients"].as_array().unwrap().iter().find(|c| c["client"] == client).cloned().unwrap()
}

/// What a control client's `run.start` does now: the gate, then the session, as the pump runs it.
fn start(h: &mut Harness<'static, NodeCraftApp>, client: AutomationClient, params: &Json) -> Result<RunGate, Error> {
    let gate = h.state_mut().automation_run_gate(client, "run.start", params, false)?;
    if gate == RunGate::Ready {
        h.state_mut().session.dispatch("run.start", params)?;
        h.state_mut().automation_run_started(client, "run.start", params);
    }
    Ok(gate)
}

#[test]
fn a_control_client_quotes_and_starts_a_paid_run_once_the_person_allows_it_once() {
    let mock = image(MockModel::new(MockOutput::Image));
    // The keychain may prompt: the maker answers only once the gate opens, and frames go on.
    let (open, gate) = std::sync::mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(gate);
    let inner = making(mock.clone());
    let mut h = scene(Box::new(move |adapter, settings| {
        let _ = gate.lock().unwrap().recv();
        inner(adapter, settings)
    }));
    // Without the gate the window's session has no route, as before #1079.
    let refused = h.state_mut().session.dispatch("run.quote", &quote_params()).unwrap();
    assert!(refused["blocked"].to_string().contains("provider route is not configured"), "{refused}");
    // The client's quote sets the provider up, and waits: nothing opens for it.
    assert_eq!(h.state_mut().automation_run_gate(CONTROL, "run.quote", &quote_params(), false).unwrap(), RunGate::Waiting(SETTING_UP));
    frames_until(&mut h, |_| true);
    assert!(h.state().run_confirm.is_none(), "no price confirmation opens for a client");
    assert!(h.state().consent_asking().is_none());
    open.send(()).unwrap();
    let start_params = quoted(&mut h);
    assert!(h.state().run_confirm.is_none(), "still nothing opened for the person's Generate");

    // The first paid start asks the person, and waits for them.
    assert_eq!(start(&mut h, CONTROL, &start_params).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label("Allow a paid run?");
    h.get_by_label_contains("The control channel wants to start a run that costs money");
    h.get_by_label_contains("It may spend up to 0.04 USD");
    // Asked again meanwhile, it still waits; another channel waits its turn.
    assert_eq!(start(&mut h, CONTROL, &start_params).unwrap(), RunGate::Waiting(ASKING));
    assert_eq!(start(&mut h, AutomationClient::McpServer, &start_params).unwrap(), RunGate::Waiting(ASKING_OTHER));
    assert!(mock.requests().is_empty(), "nothing is sent before the person answers");
    assert_eq!(list(&mut h)["asking"]["client"], "control");

    h.get_by_label("Allow once").click();
    h.run();
    assert!(h.query_by_label("Allow a paid run?").is_none(), "the answer closes the question");
    assert!(h.state().consent_asking().is_none());
    assert_eq!(decision(&mut h, "control")["decision"], "ask", "Allow once remembers nothing");
    assert_eq!(start(&mut h, CONTROL, &start_params).unwrap(), RunGate::Ready);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1, "one allowed run, one request");

    // The next paid run asks again.
    let again = quoted(&mut h);
    assert_eq!(start(&mut h, CONTROL, &again).unwrap(), RunGate::Waiting(ASKING));
}

#[test]
fn never_refuses_the_channel_with_a_clear_error_until_the_person_takes_it_back() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    let params = quoted(&mut h);
    assert_eq!(start(&mut h, CONTROL, &params).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label("Never").click();
    h.run();
    let refused = start(&mut h, CONTROL, &params).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Forbidden);
    assert_eq!(refused.message, NEVER);
    assert!(refused.message.contains("Nothing was started or charged"));
    assert_eq!(decision(&mut h, "control"), json!({"client": "control", "decision": "never", "ceiling": {}}));
    // Never is for that channel only; quoting stays free.
    assert_eq!(routed(&mut h, CONTROL, "run.quote", &quote_params()), RunGate::Ready);
    assert_eq!(start(&mut h, AutomationClient::McpServer, &params).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label("Not now").click();
    h.run();
    let declined = start(&mut h, AutomationClient::McpServer, &params).unwrap_err();
    assert_eq!((declined.kind, declined.message.as_str()), (ErrorKind::Forbidden, DECLINED));
    assert_eq!(decision(&mut h, "mcp_server")["decision"], "ask", "Not now remembers nothing");
    assert_eq!(start(&mut h, AutomationClient::McpServer, &params).unwrap(), RunGate::Waiting(ASKING), "the next try asks again");
    h.run();
    h.get_by_label("Not now").click();
    h.run();
    let _ = start(&mut h, AutomationClient::McpServer, &params);
    assert!(mock.requests().is_empty(), "nothing was sent");

    // The Automation panel takes it back.
    h.state_mut().automation.open = true;
    super::automation::tests::install_fake(&mut h);
    h.run();
    h.get_by_label_contains("The control channel: never");
    h.get_by_label("Ask again (control)").click();
    h.run();
    h.get_by_label_contains("The control channel: asks first");
    assert_eq!(decision(&mut h, "control")["decision"], "ask");
    assert_eq!(start(&mut h, CONTROL, &params).unwrap(), RunGate::Waiting(ASKING));
}

#[test]
fn always_allow_remembers_a_ceiling_and_a_run_above_it_or_of_unknown_price_asks_again() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    let params = quoted(&mut h);
    assert_eq!(start(&mut h, CONTROL, &params).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    // The ceiling starts at this run's price, and is remembered as micros.
    h.get_by_label("Always allow").click();
    h.run();
    assert_eq!(decision(&mut h, "control"), json!({"client": "control", "decision": "always", "ceiling": {"USD": 40_000}}));
    assert_eq!(start(&mut h, CONTROL, &params).unwrap(), RunGate::Ready, "a run within the ceiling starts without asking");
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1);

    // Accepting more than the ceiling asks, and says why.
    let mut more = quoted(&mut h);
    more["accept"]["amounts"]["USD"] = json!(50_000);
    assert_eq!(start(&mut h, CONTROL, &more).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label_contains("more than the 0.04 USD per run you allowed");
    h.get_by_label("Not now").click();
    h.run();
    // Not now refuses that quote's run once, however it is accepted.
    assert_eq!(start(&mut h, CONTROL, &more).unwrap_err().message, DECLINED);
    // So does an unknown part of the price, however small.
    let mut unknown = quoted(&mut h);
    unknown["accept"]["partial"] = json!(true);
    assert_eq!(start(&mut h, CONTROL, &unknown).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label_contains("isn't known yet, so it asks each time");
    h.get_by_label("Not now").click();
    h.run();
    // A start accepting no charge at all needs no consent; the engine still checks it covers
    // the quote, so this one is refused without anything sent.
    let mut free = quoted(&mut h);
    free["accept"]["amounts"]["USD"] = json!(0);
    assert_eq!(h.state_mut().automation_run_gate(AutomationClient::Agent, "run.start", &free, false).unwrap(), RunGate::Ready);
    assert!(h.state().consent_asking().is_none());
    assert!(h.state_mut().session.dispatch("run.start", &free).is_err(), "the accepted amounts must cover the quote");
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn a_budget_still_refuses_a_run_the_person_always_allows() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    h.state_mut().session.dispatch("automation.run_consent_set", &json!({"client": "control", "decision": "always", "ceiling": {"USD": 5_000_000}})).unwrap();
    let usd = |micros| nodecraft_types::Money { currency: nodecraft_types::Currency::USD, micros };
    let budgets = nodecraft_exec::cost::Budgets { session: Some(usd(10_000)), ..Default::default() };
    h.state_mut().session.swap_resource(Some(nodecraft_engine::run_cmds::BudgetState { budgets, spent: Default::default() }));
    assert_eq!(routed(&mut h, CONTROL, "run.quote", &quote_params()), RunGate::Ready);
    let quote = h.state_mut().session.dispatch("run.quote", &quote_params()).unwrap();
    assert_eq!(quote["allowed"], false, "{quote}");
    let params = json!({"canvas": C, "mode": "selected", "nodes": [N], "accept": {"token": quote["token"], "amounts": {"USD": 40_000}}});
    let refused = start(&mut h, CONTROL, &params).unwrap_err();
    assert_ne!(refused.kind, ErrorKind::Forbidden, "consent was given; the budget refuses: {refused:?}");
    assert!(mock.requests().is_empty());
}

#[test]
fn the_agents_approved_step_is_its_consent_and_an_unready_provider_is_named() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    let params = quoted(&mut h);
    assert_eq!(h.state_mut().automation_run_gate(AutomationClient::Agent, "run.start", &params, true).unwrap(), RunGate::Ready);
    assert!(h.state().consent_asking().is_none(), "the person approved this step in the sidebar");
    assert_eq!(h.state_mut().automation_run_gate(AutomationClient::Agent, "run.start", &params, false).unwrap(), RunGate::Waiting(ASKING), "auto mode asks");

    // A card whose provider isn't set up is refused, naming why, and no question is asked.
    let mut h = scene_in(MOCK_IMAGE, "other-writer", making(mock), |_, _| {});
    let refused = routed_err(&mut h);
    assert_eq!(refused.kind, ErrorKind::Unsupported, "{refused:?}");
    assert!(refused.message.contains("not in this test"), "{}", refused.message);
}

fn routed_err(h: &mut Harness<'static, NodeCraftApp>) -> Error {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match h.state_mut().automation_run_gate(CONTROL, "run.quote", &quote_params(), false) {
            Err(error) => return error,
            Ok(gate) => assert_eq!(gate, RunGate::Waiting(SETTING_UP)),
        }
        assert!(std::time::Instant::now() < deadline, "never refused");
        frames_until(h, |_| true);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn decisions_are_kept_in_settings_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(dir.path().to_path_buf()));
    let mut app = NodeCraftApp::headless().unwrap();
    app.store = Some(store);
    app.session.dispatch("automation.run_consent_set", &json!({"client": "agent", "decision": "always", "ceiling": {"USD": 2_000_000}})).unwrap();
    app.session.dispatch("automation.run_consent_set", &json!({"client": "control", "decision": "never"})).unwrap();
    app.sync_consent_settings();
    let saved = app.store.as_ref().unwrap().get().automation.run_consent.clone();
    assert_eq!(saved["agent"], nodecraft_engine::settings::RunConsentSetting::always([("USD".to_owned(), 2_000_000)].into()));
    assert_eq!(saved["control"], nodecraft_engine::settings::RunConsentSetting::never());
    // A new window reads them.
    let mut next = NodeCraftApp::headless().unwrap();
    next.load_consent(&saved);
    assert_eq!(next.session.dispatch("automation.run_consent_list", &json!({})).unwrap()["clients"][2]["decision"], "always");
    // A paid start from a client that settings say never is refused without asking, and one
    // whose params can't be read is refused rather than let past.
    let paid = json!({"canvas": next.canvas, "mode": "selected", "nodes": [], "accept": {"token": "t", "amounts": {"USD": 1}}});
    assert_eq!(next.automation_run_gate(CONTROL, "run.start", &paid, false).unwrap_err().message, NEVER);
    let hostile = json!({"canvas": next.canvas, "mode": "selected", "accept": {"token": "t", "amounts": {"USD": "1"}}});
    assert_eq!(next.automation_run_gate(AutomationClient::McpServer, "run.start", &hostile, false).unwrap_err().kind, ErrorKind::InvalidInput);
    for odd in [json!(null), json!([]), json!({"canvas": 7}), json!({"canvas": next.canvas, "mode": "sideways"})] {
        assert_eq!(next.automation_run_gate(CONTROL, "run.quote", &odd, false).unwrap(), RunGate::Ready, "the engine refuses {odd}");
    }
    assert_eq!(next.automation_run_gate(CONTROL, "node.add", &json!({}), false).unwrap(), RunGate::Ready);
}

#[test]
fn allow_once_covers_the_run_as_accepted_and_not_a_bigger_one() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    let params = quoted(&mut h);
    assert_eq!(start(&mut h, CONTROL, &params).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label_contains("It may spend up to 0.04 USD");
    h.get_by_label("Allow once").click();
    h.run();
    // The same quote, accepting more, or a part of unknown price, is a run the person didn't see.
    let mut more = params.clone();
    more["accept"]["amounts"]["USD"] = json!(500_000_000);
    assert_eq!(start(&mut h, CONTROL, &more).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label_contains("It may spend up to 500.00 USD");
    h.get_by_label("Not now").click();
    h.run();
    assert_eq!(start(&mut h, CONTROL, &more).unwrap_err().message, DECLINED);
    let mut unknown = params.clone();
    unknown["accept"]["partial"] = json!(true);
    unknown["accept"]["unpriced_jobs"] = json!(1000);
    assert_eq!(start(&mut h, CONTROL, &unknown).unwrap(), RunGate::Waiting(ASKING));
    h.run();
    h.get_by_label_contains("and 1000 job(s) of unknown price");
    h.get_by_label("Not now").click();
    h.run();
    let _ = start(&mut h, CONTROL, &unknown);
    // The run as the person saw it still starts, once.
    assert_eq!(start(&mut h, CONTROL, &params).unwrap(), RunGate::Ready);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1);
}
