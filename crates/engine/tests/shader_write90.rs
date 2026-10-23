//! Shaders written by a model (#90): `shader.write` adds a text card asked to write a custom
//! shader node's shader, as one undo step; `shader.apply` validates the card's answer and stores
//! it. No live calls: the run goes to the mock provider.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources},
    generation_catalog::GenerationCatalog,
    run_cmds::{BudgetState, RunCatalog},
    shader_assist_cmds::{FOR_CONFIG, TITLE, written_for},
};
use nodecraft_exec::cost::Budgets;
use nodecraft_graph::{Canvas, Change, Generation, Node, Position, Project};
use nodecraft_nodes::{generation::TEXT_KIND, shader};
use nodecraft_types::{CanvasId, ErrorKind, GenerationId, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([90; 16]);
const SHADER: NodeId = NodeId::from_bytes([1; 16]);
const CARD: NodeId = NodeId::from_bytes([2; 16]);
const OTHER: NodeId = NodeId::from_bytes([3; 16]);

const SEPIA: &str = "```wgsl\n// param: tone | Tone | 0 | 1 | 0.5\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    let g = dot(color.rgb, vec3<f32>(0.3, 0.59, 0.11));\n    return vec4<f32>(mix(color.rgb, vec3<f32>(g * 1.07, g * 0.74, g * 0.43), tone()), color.a);\n}\n```";

fn catalog() -> Catalog {
    Catalog::from_toml(
        r#"[[model]]
id = "fixture.writer"
provider = "mock"
provider_model = "mock-text"
label = "Fixture"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "free" }
[[model.modes]]
id = "to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#,
    )
    .unwrap()
}

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([91; 16]), "Shader writing").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = session_of(project);
    s.dispatch("node.create", &json!({"canvas":C,"id":SHADER,"kind":shader::ID,"pos":{"x":500,"y":50}})).unwrap();
    s
}

fn session_of(project: Project) -> Session {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources)
}

fn node(s: &Session, id: NodeId) -> Option<Node> {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).cloned()
}

fn prompt(node: &Node) -> String {
    match node.params.get("prompt") {
        Some(Value::Text(text)) => text.clone(),
        other => panic!("no typed prompt: {other:?}"),
    }
}

/// Gives the card `id` an answer, as a finished run would.
fn answer(s: &mut Session, id: NodeId, text: &str) {
    let mut card = node(s, id).unwrap();
    let generation = Generation {
        id: GenerationId::from_bytes([7; 16]),
        outputs: vec![Value::Text(text.into())],
        input_hash: None,
        items: Vec::new(),
        settings: Vec::new(),
        incomplete: None,
    };
    card.active = Some(generation.id);
    card.history = vec![generation];
    let mut project = s.document().clone();
    Change::SetNode { canvas: C, node: card }.apply(&mut project).unwrap();
    *s = session_of(project);
}

fn refused(s: &mut Session, command: &str, params: &Json) -> nodecraft_types::Error {
    let before = s.revision();
    let error = s.dispatch(command, params).unwrap_err();
    assert_eq!(s.revision(), before, "a refused {command} changes nothing");
    error
}

#[test]
fn writing_adds_a_linked_text_card_asking_for_the_shader_as_one_undo_step() {
    let mut s = session();
    let out = s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"warm sepia tones"})).unwrap();
    assert_eq!(out, json!({"id": CARD, "model": "fixture.writer"}));
    let card = node(&s, CARD).unwrap();
    assert_eq!(card.kind, TEXT_KIND);
    assert_eq!(card.title, TITLE);
    assert_eq!(written_for(&card), Some(SHADER));
    assert_eq!(card.pos, Position { x: 100.0, y: 50.0 }, "left of the shader node");
    let asked = prompt(&card);
    assert!(asked.contains("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>"), "{asked}");
    assert!(asked.contains("}\n// param: amount | Amount | 0 | 1 | 1\n```"), "the current shader is shown: {asked}");
    assert!(asked.ends_with("warm sepia tones\n"), "{asked}");
    assert!(card.history.is_empty(), "nothing runs");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, CARD).is_none(), "one undo step");
}

#[test]
fn a_fix_passes_on_the_compiler_message() {
    let mut s = session();
    s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"error":"line 2: unknown identifier `tint`"})).unwrap();
    let asked = prompt(&node(&s, CARD).unwrap());
    assert!(asked.contains("unknown identifier `tint`") && asked.contains("change only what the message asks for"), "{asked}");
}

#[test]
fn a_fix_shows_the_shader_the_message_is_about_not_the_stored_one() {
    let mut s = session();
    let draft = json!({"source": "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return tint;\n}\n", "params": []});
    let error = "wgsl:2:12 unknown identifier `tint`";
    s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"error":error,"shader":draft})).unwrap();
    let asked = prompt(&node(&s, CARD).unwrap());
    assert!(asked.contains("```wgsl\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return tint;\n}\n```"), "line 2 is the draft's: {asked}");
    assert!(!asked.contains("amount()"), "the stored shader isn't shown: {asked}");
    let huge = json!({"source": "a".repeat(nodecraft_gpu::shader::MAX_SOURCE + 1), "params": []});
    let refusal = refused(&mut s, "shader.write", &json!({"canvas":C,"node":SHADER,"id":OTHER,"error":error,"shader":huge}));
    assert_eq!(refusal.kind, ErrorKind::InvalidInput);
}

#[test]
fn bad_writes_are_refused() {
    let mut s = session();
    let neither = refused(&mut s, "shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD}));
    assert_eq!(neither.kind, ErrorKind::InvalidInput);
    let blank = refused(&mut s, "shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"  "}));
    assert!(blank.message.contains("not only spaces"));
    let long = refused(&mut s, "shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"a".repeat(4097)}));
    assert_eq!(long.kind, ErrorKind::InvalidInput);
    let taken = refused(&mut s, "shader.write", &json!({"canvas":C,"node":SHADER,"id":SHADER,"request":"blur"}));
    assert_eq!(taken.kind, ErrorKind::Conflict);
    let missing = refused(&mut s, "shader.write", &json!({"canvas":C,"node":OTHER,"id":CARD,"request":"blur"}));
    assert_eq!(missing.kind, ErrorKind::NotFound);
    let model = refused(&mut s, "shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"blur","model":"fixture.none"}));
    assert_eq!(model.kind, ErrorKind::NotFound);
    s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"blur"})).unwrap();
    let not_shader = refused(&mut s, "shader.write", &json!({"canvas":C,"node":CARD,"id":OTHER,"request":"blur"}));
    assert!(not_shader.message.contains("only a custom shader node"), "{not_shader:?}");
}

#[test]
fn applying_stores_a_valid_answer_with_its_sliders_as_one_undo_step() {
    let mut s = session();
    s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"sepia"})).unwrap();
    let missing = refused(&mut s, "shader.apply", &json!({"canvas":C,"id":SHADER,"from":CARD}));
    assert!(missing.message.contains("run it first"), "{missing:?}");
    answer(&mut s, CARD, &format!("Sure:\n{SEPIA}\n"));
    assert_eq!(s.dispatch("shader.apply", &json!({"canvas":C,"id":SHADER,"from":CARD})).unwrap(), json!({"changed": true}));
    let stored = shader::stored(&node(&s, SHADER).unwrap()).unwrap();
    assert_eq!(stored.params.len(), 1);
    assert_eq!(stored.params[0].name, "tone");
    assert!(stored.source.contains("tone()"));
    assert_eq!(s.dispatch("shader.apply", &json!({"canvas":C,"id":SHADER,"from":CARD})).unwrap(), json!({"changed": false}));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(shader::stored(&node(&s, SHADER).unwrap()).unwrap(), shader::StoredShader::default());
}

#[test]
fn an_invalid_answer_is_refused_with_the_compiler_message() {
    let mut s = session();
    s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"request":"loop"})).unwrap();
    answer(&mut s, CARD, "```wgsl\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return tint(); }\n```");
    let error = refused(&mut s, "shader.apply", &json!({"canvas":C,"id":SHADER,"from":CARD}));
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.starts_with("the shader isn't valid"), "{error:?}");
    answer(&mut s, CARD, "// param: x | X | 0 | 1\nfn effect");
    assert!(refused(&mut s, "shader.apply", &json!({"canvas":C,"id":SHADER,"from":CARD})).message.contains("isn't a shader"));
    let not_shader = refused(&mut s, "shader.apply", &json!({"canvas":C,"id":CARD,"from":CARD}));
    assert!(not_shader.message.contains("only a custom shader node"), "{not_shader:?}");
    let config = node(&s, CARD).unwrap().config;
    assert!(config.contains_key(FOR_CONFIG));
}

mod run {
    //! The request card runs like any other card, quoted first; its answer becomes the shader.
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
    use nodecraft_providers::mock::MockProvider;
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

    #[test]
    fn the_request_card_is_quoted_run_and_its_answer_applied() {
        let mut s = session();
        // The node's shader is sepia when the request is written.
        let sepia = nodecraft_engine::shader_assist_cmds::parse_reply(SEPIA).unwrap();
        s.dispatch("node.shader_set", &json!({"canvas":C,"id":SHADER,"source":sepia.source,"params":sepia.params})).unwrap();
        s.dispatch("shader.write", &json!({"canvas":C,"node":SHADER,"id":CARD,"error":"make it compile"})).unwrap();
        // Then it changes back, so applying the answer is a visible change.
        s.dispatch(
            "node.shader_set",
            &json!({"canvas":C,"id":SHADER,"source":shader::StoredShader::default().source,"params":shader::StoredShader::default().params}),
        )
        .unwrap();

        let mock = MockProvider::standard();
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: "shader-write".into(),
            wall_clock_ms: Arc::new(|| 0),
        };
        let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
        let mut services = Services::default();
        host.register(&mut services).unwrap();
        s = s.with_services(services);
        let mut routing = ProviderRouting::new();
        let binding = routing.bind_default(Arc::new(mock.clone())).unwrap();
        routing.select(CARD, "mock", binding.target()).unwrap();
        s.set_provider_routing(routing).unwrap();
        host.install(&mut s);

        let request = json!({"canvas":C,"mode":"selected","nodes":[CARD]});
        let quote = s.dispatch("run.quote", &request).unwrap();
        assert_eq!(quote["allowed"], true, "{quote}");
        let mut start = request;
        start["accept"] = json!({"token":quote["token"],"partial":quote["complete"] != true,"unpriced_jobs":0,"amounts":{}});
        s.dispatch("run.start", &start).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut notices = Vec::new();
        loop {
            notices.extend(host.pump(&mut s));
            if !host.is_busy() {
                break;
            }
            assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(notices.iter().any(|n| matches!(n, RunNotice::Recorded { .. })), "{notices:?}");
        assert_eq!(mock.requests().len(), 1);
        assert_eq!(mock.requests()[0].model, "mock-text");

        // The mock answers with the prompt it was sent, whose one code block is the shader as
        // it was when the request was written.
        assert_eq!(s.dispatch("shader.apply", &json!({"canvas":C,"id":SHADER,"from":CARD})).unwrap(), json!({"changed": true}));
        let applied = shader::stored(&node(&s, SHADER).unwrap()).unwrap();
        assert_eq!(applied.params, sepia.params);
        assert!(applied.source.contains("tone()"));
    }
}
