//! ⌘/Ctrl+Enter and Run workflow (#965): a run of the whole canvas is quoted and confirmed like a
//! card's own run, and a confirmed run starts exactly the cards it quoted.
use super::*;

const M: NodeId = NodeId::from_bytes([88; 16]);

/// A result recorded on `id` that matches its inputs now, so it is fresh.
fn record_fresh(h: &mut Harness<'static, NodeCraftApp>, id: NodeId, n: u8) {
    let hash = {
        let app = h.state();
        let canvas = app.session.document().canvases().get(&C).unwrap();
        nodecraft_exec::staleness::input_hash(canvas, app.session.kinds(), id).unwrap()
    };
    let generation = nodecraft_graph::Generation {
        id: nodecraft_types::GenerationId::from_bytes([n; 16]),
        outputs: vec![Value::Text("An answer".into())],
        input_hash: Some(hash),
        items: Vec::new(),
        settings: Vec::new(),
        incomplete: None,
        source_versions: Vec::new(),
    };
    h.state_mut().session.dispatch("run.record", &json!({"canvas": C, "results": [{"node": id, "generation": generation}]})).unwrap();
    h.run();
}

/// The scene's card N with no result, and a second card M (same model) holding a fresh one;
/// both routed to the mock provider.
fn two_cards() -> (Harness<'static, NodeCraftApp>, Runs) {
    let (mut h, runs) = scene("mock-square");
    let mut second = node(h.state()).clone();
    second.id = M;
    second.title = "Second card".into();
    second.pos = nodecraft_graph::Position { x: 600.0, y: 120.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": C, "node": second})).unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(nodecraft_provider_testkit::mock::MockProvider::standard())).unwrap();
    routing.select(N, "mock", binding.target()).unwrap();
    routing.select(M, "mock", binding.target()).unwrap();
    h.state_mut().session.set_provider_routing(routing).unwrap();
    record_fresh(&mut h, M, 89);
    (h, runs)
}

fn ctrl_enter(h: &mut Harness<'static, NodeCraftApp>) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
}

fn started(runs: &Runs) -> Vec<Vec<NodeId>> {
    runs.lock().unwrap().iter().map(|run| run.quote().steps.iter().map(|step| step.node).collect()).collect()
}

#[test]
fn ctrl_enter_quotes_what_changed_and_starts_nothing_until_confirmed() {
    let (mut h, runs) = two_cards();
    assert!(!canvas::text_input_focused(&h.ctx), "the shortcut is pressed on the canvas, not in a field");
    ctrl_enter(&mut h);
    let confirm = h.state().run_confirm.as_ref().expect("Cmd/Ctrl+Enter asks before running");
    assert_eq!(confirm.nodes(), [N], "the card with no result; fresh M is left alone");
    assert!(runs.lock().unwrap().is_empty(), "a quote never starts a run");
    assert!(h.query_by_label_contains("Runs 1 card: A red square.").is_some(), "the dialog names what runs");
    let accept = h.get_by_label_contains("Run changed for").accesskit_node().label().unwrap_or_default();
    assert!(accept.contains("0.04"), "the total is shown before anything runs: {accept}");
    press(&mut h, Role::Button, "Cancel");
    assert!(h.state().run_confirm.is_none());
    assert!(runs.lock().unwrap().is_empty(), "Cancel starts nothing");

    ctrl_enter(&mut h);
    press(&mut h, Role::Button, &accept);
    assert_eq!(started(&runs), vec![vec![N]], "the confirmed run starts exactly the quoted card");
    assert!(h.state().run_confirm.is_none());
}

#[test]
fn run_workflow_is_in_the_palette_and_runs_every_generator_through_the_same_quote() {
    let (mut h, runs) = two_cards();
    let entries = h.state().palette_entries();
    let entry = |id: &'static str| entries.iter().find(|entry| entry.target == palette::Target::Command(id));
    let changed = entry("run.changed").expect("Run changed is in the palette");
    assert_eq!((changed.label.as_str(), changed.shortcut.as_deref()), ("Run changed", Some("Command+Enter")));
    assert_eq!(entry("run.workflow").expect("Run workflow is in the palette").label, "Run workflow");

    h.state_mut().dispatch_palette_command("run.workflow");
    h.run();
    let confirm = h.state().run_confirm.as_ref().expect("Run workflow asks before running");
    let mut nodes = confirm.nodes().to_vec();
    nodes.sort();
    assert_eq!(nodes, [N, M], "fresh M runs again in the workflow");
    assert!(h.query_by_label_contains("Runs 2 cards:").is_some());
    let accept = h.get_by_label_contains("Run workflow for").accesskit_node().label().unwrap_or_default();
    assert!(accept.contains("0.08"), "both cards are priced: {accept}");
    assert!(runs.lock().unwrap().is_empty());
    press(&mut h, Role::Button, &accept);
    assert_eq!(started(&runs), vec![vec![N, M]], "the confirmed workflow starts exactly the quoted cards");
}

#[test]
fn nothing_to_run_says_so_without_a_dialog() {
    let (mut h, runs) = two_cards();
    record_fresh(&mut h, N, 90);
    ctrl_enter(&mut h);
    assert!(h.state().run_confirm.is_none());
    assert_eq!(h.state().notice.as_deref(), Some(generate::NOTHING_TO_RUN));
    // An empty canvas has no workflow either.
    for id in [N, M] {
        h.state_mut().session.dispatch("node.remove", &json!({"canvas": C, "id": id})).unwrap();
    }
    h.state_mut().dispatch_palette_command("run.workflow");
    h.run();
    assert!(h.state().run_confirm.is_none());
    assert_eq!(h.state().notice.as_deref(), Some(generate::NO_WORKFLOW));
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn ctrl_enter_in_a_prompt_still_quotes_only_that_card() {
    let (mut h, runs) = scene("mock-square");
    h.get_by_role_and_label(Role::TextInput, "Prompt").focus();
    h.run();
    ctrl_enter(&mut h);
    assert!(h.state().run_confirm.is_some(), "Cmd+Enter in a prompt asks to generate its card");
    assert!(h.query_by_label_contains("Generate for").is_some(), "the card's own Generate, not Run changed");
    assert!(h.query_by_label_contains("Run changed for").is_none());
    assert!(runs.lock().unwrap().is_empty());
}
