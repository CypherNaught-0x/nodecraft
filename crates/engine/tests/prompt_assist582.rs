//! Prompt assistance (#582): `node.prompt_assist` adds a text card that rewrites a Generate card's
//! typed prompt and wires its answer into that prompt, as one undo step. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources},
    generation_catalog::GenerationCatalog,
    run_cmds::{BudgetState, RunCatalog},
};
use nodecraft_exec::cost::Budgets;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Node, Position, Project};
use nodecraft_nodes::generation::{KIND, TEXT_KIND};
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([82; 16]);
const PAINTER: NodeId = NodeId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([2; 16]);
const ASSIST: NodeId = NodeId::from_bytes([3; 16]);
const WRITER: NodeId = NodeId::from_bytes([4; 16]);
const WIRE: EdgeId = EdgeId::from_bytes([5; 16]);

const PROMPT_ROLE: &str = r#"{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}"#;

fn model(id: &str, output: &str, image_input: bool) -> String {
    let image = if image_input { r#", {{ role = "image", modality = "image", min = 1, max = 4 }}"# } else { "" };
    model_with(id, output, &format!("{PROMPT_ROLE}{image}"))
}

fn model_with(id: &str, output: &str, inputs: &str) -> String {
    format!(
        r#"[[model]]
id = "{id}"
provider = "mock"
provider_model = "mock-{output}"
label = "Fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "free" }}
[[model.modes]]
id = "to-{output}"
output = "{output}"
inputs = [{inputs}]
"#
    )
    .replace("{{", "{")
    .replace("}}", "}")
}

/// An image model, two text models (`fixture.b` sorts after `fixture.a`) and, when `vision`, text
/// models that sort first but can't answer a typed prompt alone: one needs an image, one reads a
/// wired `text` role instead of a prompt (as `replicate.hello` does), and one takes its prompt
/// only by wire.
fn catalog(text: bool, vision: bool) -> Catalog {
    let mut catalog = Catalog::from_toml(&model("fixture.painter", "image", false)).unwrap();
    let mut add = |toml: String| catalog.merge(Catalog::from_toml(&toml).unwrap()).unwrap();
    if text {
        add(model("fixture.a", "text", false));
        add(model("fixture.b", "text", false));
    }
    if vision {
        add(model("fixture.0vision", "text", true));
        add(model_with("fixture.0hello", "text", r#"{{ role = "text", modality = "text", min = 1, max = 1 }}"#));
        add(model_with("fixture.0wired", "text", r#"{{ role = "prompt", modality = "text", min = 1, max = 1, inline = false }}"#));
    }
    catalog
}

fn session_with(catalog: Catalog, prompt: &str) -> Session {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([83; 16]), "Prompt assist").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(KIND)
        .unwrap()
        .instantiate_instance(
            PAINTER,
            Default::default(),
            &[("model".into(), Value::Text("fixture.painter".into())), ("prompt".into(), Value::Text(prompt.into()))].into(),
        )
        .unwrap();
    node.pos = Position { x: 100.0, y: 50.0 };
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources)
}

fn session(prompt: &str) -> Session {
    session_with(catalog(true, false), prompt)
}

fn request() -> Json {
    json!({"canvas":C,"node":PAINTER,"id":ASSIST,"edge":WIRE})
}

fn node(s: &Session, id: NodeId) -> Option<Node> {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).cloned()
}

fn text(node: &Node, name: &str) -> String {
    match node.params.get(name) {
        Some(Value::Text(text)) => text.clone(),
        other => panic!("{name} is not text: {other:?}"),
    }
}

fn wired(s: &Session) -> Vec<Edge> {
    s.document().canvases().get(&C).unwrap().edges().values().filter(|edge| edge.to.node == PAINTER).cloned().collect()
}

fn add(s: &mut Session, kind: &str, id: NodeId, params: &[(&str, &str)]) {
    let values = params.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).into()))).collect();
    let node = s.kinds().get(kind).unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
    s.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
}

#[test]
fn assisting_adds_a_text_card_wired_into_the_prompt_as_one_undo_step() {
    let mut s = session("a fox in snow @[Sky](00000000-0000-0000-0000-000000000000)");
    let out = s.dispatch("node.prompt_assist", &request()).unwrap();
    assert_eq!(out["id"], json!(ASSIST));
    assert_eq!(out["edge"], json!(WIRE));
    assert_eq!(out["model"], "fixture.a", "the first installed text model");
    let assist = node(&s, ASSIST).unwrap();
    assert_eq!(assist.kind, TEXT_KIND);
    assert_eq!(assist.title, "Improved prompt");
    assert_eq!(assist.pos, Position { x: -300.0, y: 50.0 }, "left of the card it feeds");
    let prompt = text(&assist, "prompt");
    assert!(prompt.starts_with("Rewrite the image prompt below"), "{prompt}");
    assert!(prompt.ends_with("a fox in snow @[Sky](00000000-0000-0000-0000-000000000000)"), "the prompt and its mentions are carried over: {prompt}");
    assert_eq!(
        wired(&s),
        [Edge { id: WIRE, from: Endpoint { node: ASSIST, port: "out".into() }, to: Endpoint { node: PAINTER, port: "prompt".into() }, order: 0 }]
    );
    // The typed prompt stays on the image card for when the wire goes.
    assert!(text(&node(&s, PAINTER).unwrap(), "prompt").starts_with("a fox in snow"));

    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, ASSIST).is_none(), "one undo removes the card");
    assert!(wired(&s).is_empty(), "and its wire");
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert!(node(&s, ASSIST).is_some());
    assert_eq!(wired(&s).len(), 1);
}

#[test]
fn a_custom_instruction_replaces_the_standard_rewrite() {
    let mut s = session("a fox");
    let mut req = request();
    req["instruction"] = json!("  Make it moodier.  ");
    s.dispatch("node.prompt_assist", &req).unwrap();
    assert_eq!(text(&node(&s, ASSIST).unwrap(), "prompt"), "Make it moodier.\nReply with the rewritten prompt only.\n\n---\na fox");
}

#[test]
fn a_text_card_on_the_canvas_chooses_the_model_and_an_asked_model_wins() {
    let mut s = session("a fox");
    add(&mut s, TEXT_KIND, WRITER, &[("model", "fixture.b")]);
    let out = s.dispatch("node.prompt_assist", &request()).unwrap();
    assert_eq!(out["model"], "fixture.b", "the model already in use here");
    s.dispatch("edit.undo", &json!({})).unwrap();
    let mut req = request();
    req["model"] = json!("fixture.a");
    assert_eq!(s.dispatch("node.prompt_assist", &req).unwrap()["model"], "fixture.a");
}

#[test]
fn a_text_card_gets_the_text_instruction() {
    let mut s = session("a fox");
    add(&mut s, TEXT_KIND, WRITER, &[("model", "fixture.b"), ("prompt", "a poem")]);
    s.dispatch("node.prompt_assist", &json!({"canvas":C,"node":WRITER,"id":ASSIST,"edge":WIRE})).unwrap();
    let prompt = text(&node(&s, ASSIST).unwrap(), "prompt");
    assert!(prompt.starts_with("Rewrite the instruction below so a language model"), "{prompt}");
    assert!(prompt.ends_with("a poem"));
}

fn refused(s: &mut Session, req: &Json) -> nodecraft_types::Error {
    let before = s.document().clone();
    assert!(s.enabled("node.prompt_assist", req).is_err(), "disabled: {req}");
    let err = s.dispatch("node.prompt_assist", req).unwrap_err();
    assert_eq!(s.document(), &before, "a refusal changes nothing");
    err
}

#[test]
fn an_empty_prompt_is_refused() {
    let mut s = session("   ");
    assert!(refused(&mut s, &request()).message.contains("write a prompt first"));
}

#[test]
fn a_connected_prompt_is_refused() {
    let mut s = session("a fox");
    add(&mut s, "source.text", SOURCE, &[]);
    let edge = Edge {
        id: EdgeId::from_bytes([9; 16]),
        from: Endpoint { node: SOURCE, port: "out".into() },
        to: Endpoint { node: PAINTER, port: "prompt".into() },
        order: 0,
    };
    s.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    assert_eq!(refused(&mut s, &request()).kind, ErrorKind::Conflict);
}

#[test]
fn bad_params_are_refused() {
    let mut s = session("a fox");
    add(&mut s, "source.text", SOURCE, &[]);
    let mut not_generation = request();
    not_generation["node"] = json!(SOURCE);
    assert!(refused(&mut s, &not_generation).message.contains("Generate card"));
    let mut missing = request();
    missing["node"] = json!(NodeId::from_bytes([77; 16]));
    assert_eq!(refused(&mut s, &missing).kind, ErrorKind::NotFound);
    let mut taken = request();
    taken["id"] = json!(SOURCE);
    assert_eq!(refused(&mut s, &taken).kind, ErrorKind::Conflict);
    for instruction in [json!("   "), json!("x".repeat(4097))] {
        let mut req = request();
        req["instruction"] = instruction;
        assert_eq!(refused(&mut s, &req).kind, ErrorKind::InvalidInput);
    }
    let mut image_model = request();
    image_model["model"] = json!("fixture.painter");
    assert_eq!(refused(&mut s, &image_model).kind, ErrorKind::NotFound, "an image model can't be the assist card's");
    let mut extra = request();
    extra["surprise"] = json!(1);
    refused(&mut s, &extra);
}

#[test]
fn a_text_model_that_needs_an_image_is_never_chosen() {
    let mut s = session_with(catalog(true, true), "a fox");
    assert_eq!(s.dispatch("node.prompt_assist", &request()).unwrap()["model"], "fixture.a", "the fixture.0 models sort first but can't answer alone");
    s.dispatch("edit.undo", &json!({})).unwrap();
    for model in ["fixture.0vision", "fixture.0hello", "fixture.0wired"] {
        let mut req = request();
        req["model"] = json!(model);
        assert!(refused(&mut s, &req).message.contains("text-only mode"), "{model}");
    }
}

#[test]
fn a_prompt_too_long_to_improve_is_refused() {
    let limit = nodecraft_nodes::generation::MAX_PROMPT_BYTES;
    let mut s = session(&"a".repeat(limit - 100));
    assert!(!nodecraft_engine::assist_cmds::fits(KIND, &"a".repeat(limit - 100)));
    assert!(refused(&mut s, &request()).message.contains("too long to improve"));
    assert!(nodecraft_engine::assist_cmds::fits(KIND, &"a".repeat(limit / 2)));
}

#[test]
fn without_a_text_model_the_command_says_so() {
    let mut s = session_with(catalog(false, false), "a fox");
    assert!(refused(&mut s, &request()).message.contains("no text model is installed"));
}

mod run {
    //! The assist card runs like any other card, quoted first, and its answer becomes the image
    //! card's prompt.
    use super::*;
    use nodecraft_engine::{
        commands::Services,
        jobs::JobBudget,
        provider_routing::ProviderRouting,
        run_host::{RunHost, RunNotice},
    };
    use nodecraft_exec::{
        cost::UsageLog,
        driver::{Runtime, TaskFuture},
        scheduler::RetryPolicy,
    };
    use nodecraft_providers::{JobInput, mock::MockProvider};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    struct Threads(Instant);
    impl Runtime for Threads {
        fn spawn(&self, task: TaskFuture<()>) {
            std::thread::spawn(move || futures::executor::block_on(task));
        }
        fn sleep(&self, ms: u64) -> TaskFuture<()> {
            let (tx, rx) = futures::channel::oneshot::channel::<()>();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(ms));
                let _ = tx.send(());
            });
            Box::pin(async move {
                let _ = rx.await;
            })
        }
        fn now_ms(&self) -> u64 {
            u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
        }
    }

    fn start(s: &mut Session, host: &mut RunHost, node: NodeId) {
        let request = json!({"canvas":C,"mode":"selected","nodes":[node]});
        let q = s.dispatch("run.quote", &request).unwrap();
        assert_eq!(q["allowed"], true, "{q}");
        let mut start = request;
        start["accept"] = json!({"token":q["token"],"partial":q["complete"] != true,"unpriced_jobs":0,"amounts":{}});
        s.dispatch("run.start", &start).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut notices = Vec::new();
        loop {
            notices.extend(host.pump(s));
            if !host.is_busy() {
                break;
            }
            assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    }

    fn prompt_of(job: &nodecraft_providers::ProviderJob) -> String {
        job.inputs.iter().find_map(|input| if let JobInput::Text { role, text } = input { (role == "prompt").then(|| text.clone()) } else { None }).unwrap()
    }

    #[test]
    fn the_assist_card_answers_and_its_answer_prompts_the_image_card() {
        let mut s = session("a fox in snow");
        // The prompt mentions a text card (#505); the mention moves to the assist card with it.
        add(&mut s, "source.text", SOURCE, &[("text", "snowy pines")]);
        let typed = format!("a fox beside {}", nodecraft_nodes::mention::token("Pines", SOURCE));
        s.dispatch("node.param_set", &json!({"canvas":C,"id":PAINTER,"name":"prompt","value":{"type":"text","value":typed}})).unwrap();
        let mock = MockProvider::standard();
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: "prompt-assist".into(),
            wall_clock_ms: Arc::new(|| 0),
        };
        let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
        let mut services = Services::default();
        host.register(&mut services).unwrap();
        s = s.with_services(services);
        let mut routing = ProviderRouting::new();
        let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
        for node in [PAINTER, ASSIST] {
            routing.select(node, "mock", binding.target()).unwrap();
        }
        s.set_provider_routing(routing).unwrap();
        host.install(&mut s);
        s.dispatch("node.prompt_assist", &request()).unwrap();
        assert!(mock.requests().is_empty(), "adding the card runs nothing");

        start(&mut s, &mut host, ASSIST);
        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "mock-text");
        let asked = prompt_of(&requests[0]);
        assert!(asked.starts_with("Rewrite the image prompt below") && asked.ends_with("a fox beside snowy pines"), "the mention is read: {asked}");
        let assist = node(&s, ASSIST).unwrap();
        let answer = assist.history.iter().find(|g| g.id == assist.active.unwrap()).unwrap().outputs[0].clone();
        let Value::Text(answer) = answer else { panic!("a text answer: {answer:?}") };

        start(&mut s, &mut host, PAINTER);
        let requests = mock.requests();
        assert_eq!(requests.len(), 2, "the answered card isn't run again");
        assert_eq!(requests[1].model, "mock-image");
        assert_eq!(prompt_of(&requests[1]), answer, "the improved prompt replaces the typed one");
        assert_eq!(requests[1].inputs.iter().filter(|input| matches!(input, JobInput::Text { .. })).count(), 1, "the wired prompt alone, no mention");
        assert_eq!(text(&node(&s, PAINTER).unwrap(), "prompt"), typed, "the typed prompt is kept");
    }
}
