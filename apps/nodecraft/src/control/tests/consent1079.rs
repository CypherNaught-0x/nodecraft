//! A control client's paid `run.start` waits in the pump while the window asks the person
//! (#1079), and is answered once they choose: Allow once lets it reach the session, Never refuses
//! it with a clear error. The person's answers are real pointer clicks on the dialog.
use super::*;
use crate::shell::Shell;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_ui_egui::app::automation_runs::NEVER;

fn shell(calls: std::sync::mpsc::Receiver<Call>) -> Harness<'static, Shell> {
    let mut pending = Some(calls);
    Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_| {
        let app = NodeCraftApp::headless().unwrap();
        let listener = control::Listener { addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)), calls: pending.take().unwrap() };
        let channel = Channel { listener, pump: CallPump::new(FileRoots::default(), AutomationClient::Control), token_file: None };
        let agent = crate::agent_panel::AgentPanel::new(Err("no agent in this test".into()), Default::default());
        Shell { app, agent, channel: Some(channel), storage_window: None, storage_window_pending: None, mcp: None }
    })
}

/// A paid start, sent from another thread as a client would; the reply arrives on the handle.
type Reply = std::thread::JoinHandle<(control::LocalClient, Result<serde_json::Value, nodecraft_types::Error>)>;

fn paid_start(h: &Harness<'static, Shell>, mut client: control::LocalClient) -> Reply {
    let canvas = h.state().app.canvas;
    std::thread::spawn(move || {
        let params = json!({"id": "run.start", "params": {"canvas": canvas, "mode": "selected", "nodes": [],
            "accept": {"token": "0".repeat(64), "amounts": {"USD": 40_000}}}});
        let result = client.call("command.run", params.as_object().unwrap().clone());
        (client, result)
    })
}

/// Frames until the window asks.
fn until_asked(h: &mut Harness<'static, Shell>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.query_by_label("Allow a paid run?").is_none() {
        assert!(std::time::Instant::now() < deadline, "the window never asked");
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // A widget takes no click on the frame it first appears.
    for _ in 0..3 {
        h.step();
    }
}

/// Frames until the client has its answer.
fn answered(h: &mut Harness<'static, Shell>, reply: Reply) -> (control::LocalClient, Result<serde_json::Value, nodecraft_types::Error>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !reply.is_finished() {
        assert!(std::time::Instant::now() < deadline, "the client was never answered");
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    reply.join().unwrap()
}

#[test]
fn a_paid_start_waits_for_the_person_and_allow_once_lets_it_reach_the_session() {
    let (client, calls) = control::local(Arc::new(|| {}));
    let mut h = shell(calls);
    let reply = paid_start(&h, client);
    until_asked(&mut h);
    h.get_by_label_contains("The control channel wants to start a run that costs money");
    assert!(!reply.is_finished(), "the call waits for the person, parked in the pump");
    h.get_by_label("Allow once").click();
    let (_, result) = answered(&mut h, reply);
    // It reached the session, which checks the quote as always: this token was never quoted.
    let error = result.unwrap_err();
    assert_ne!(error.kind, nodecraft_types::ErrorKind::Forbidden, "{error:?}");
    assert_ne!(error.kind, nodecraft_types::ErrorKind::Busy, "{error:?}");
    assert!(h.query_by_label("Allow a paid run?").is_none());
}

#[test]
fn never_answers_the_waiting_client_with_a_clear_refusal_and_refuses_the_next_at_once() {
    let (client, calls) = control::local(Arc::new(|| {}));
    let mut h = shell(calls);
    let reply = paid_start(&h, client);
    until_asked(&mut h);
    h.get_by_label("Never").click();
    let (client, result) = answered(&mut h, reply);
    let error = result.unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (nodecraft_types::ErrorKind::Forbidden, NEVER));
    // The next one is refused without asking.
    let next = paid_start(&h, client);
    let (_, again) = answered(&mut h, next);
    assert_eq!(again.unwrap_err().message, NEVER);
    assert!(h.query_by_label("Allow a paid run?").is_none());
    let listed = h.state_mut().app.session.dispatch("automation.run_consent_list", &json!({})).unwrap();
    assert_eq!(listed["clients"][0]["decision"], "never", "{listed}");
}

#[test]
fn a_client_cannot_answer_its_own_question() {
    let (client, calls) = control::local(Arc::new(|| {}));
    let mut h = shell(calls);
    let _waiting = paid_start(&h, client);
    until_asked(&mut h);
    // The commands that give consent are the person's: automation is refused them.
    let mut connection = super::connection();
    for (id, params) in [
        ("automation.run_consent_answer", json!({"answer": "once"})),
        ("automation.run_consent_set", json!({"client": "control", "decision": "always", "ceiling": {"USD": 1_000_000}})),
    ] {
        let request = super::request(&mut connection, "command.run", json!({"id": id, "params": params}));
        let refused = answer(&mut h.state_mut().app, &egui::Context::default(), &FileRoots::default(), &request).unwrap().unwrap_err();
        assert_eq!(refused.kind, nodecraft_types::ErrorKind::Unsupported, "{id}: {refused:?}");
        assert!(refused.message.contains("only be run by a person"), "{}", refused.message);
    }
    // The list is the client's to read.
    let request = super::request(&mut connection, "command.run", json!({"id": "automation.run_consent_list", "params": {}}));
    let listed = answer(&mut h.state_mut().app, &egui::Context::default(), &FileRoots::default(), &request).unwrap().unwrap();
    assert_eq!(listed["result"]["asking"]["client"], "control", "{listed}");
    assert!(h.query_by_label("Allow a paid run?").is_some(), "still asking");
}
