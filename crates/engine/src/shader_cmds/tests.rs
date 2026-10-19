use crate::Session;
use nodecraft_types::{CanvasId, NodeId, Value};
use serde_json::json;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const SHADER: NodeId = NodeId::from_bytes([5; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": SHADER, "kind": "image.shader", "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

fn sliders(s: &Session) -> Vec<String> {
    let node = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&SHADER).unwrap();
    s.kinds().get("image.shader").unwrap().resolve_instance(node).unwrap().params.params.iter().map(|p| p.name.clone()).collect()
}

const POSTERIZE: &str = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    let n = max(levels(), 2.0) - 1.0;
    return vec4<f32>(floor(color.rgb * n + 0.5) / n, color.a);
}";

#[test]
fn a_valid_shader_is_stored_and_its_sliders_follow() {
    let mut s = session();
    assert_eq!(sliders(&s), ["amount"], "a fresh node inverts");
    let set = json!({"canvas": CANVAS, "id": SHADER, "source": POSTERIZE, "params": [{"name": "levels", "label": "Levels", "min": 2.0, "max": 16.0, "default": 4.0}]});
    assert_eq!(s.dispatch("node.shader_set", &set).unwrap(), json!({"changed": true}));
    assert_eq!(sliders(&s), ["levels"]);
    assert_eq!(s.dispatch("node.shader_set", &set).unwrap(), json!({"changed": false}), "the same shader again changes nothing");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(sliders(&s), ["amount"], "one undo step");
}

#[test]
fn an_invalid_shader_is_refused_with_the_compiler_s_reason_and_changes_nothing() {
    let mut s = session();
    let before = s.document().clone();
    for (source, why) in [
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { loop { } return color; }", "loop"),
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return colour; }", "colour"),
    ] {
        let error = s.dispatch("node.shader_set", &json!({"canvas": CANVAS, "id": SHADER, "source": source})).unwrap_err();
        assert!(error.message.contains(why), "{}", error.message);
    }
    assert_eq!(s.document(), &before);
    // Only shader nodes take one.
    let text = NodeId::from_bytes([6; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": text, "kind": "source.text", "pos": {"x": 0, "y": 0}})).unwrap();
    assert!(s.dispatch("node.shader_set", &json!({"canvas": CANVAS, "id": text, "source": POSTERIZE})).is_err());
}

#[test]
#[ignore = "requires an actual GPU; run explicitly"]
fn a_shader_node_runs_live_on_the_gpu() {
    use crate::live_actions::{apply, compute, pending};
    let mut s = session();
    let pixels: Vec<u8> = (0..16 * 12).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    let png = nodecraft_media::encode_png(16, 12, &pixels, &nodecraft_media::Limits::default()).unwrap();
    let hash = s.import_asset(&png).unwrap();
    let source = NodeId::from_bytes([7; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": source, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": source, "name": "asset", "value": Value::Media(asset)})).unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([8; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": source, "port": "out"}, "to": {"node": SHADER, "port": "image"}, "order": 0}}),
    )
    .unwrap();
    let job = pending(&s, CANVAS, &Default::default()).unwrap().unwrap();
    let outcome = compute(job, &nodecraft_nodes::Registry::builtins().unwrap()).unwrap();
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert!(apply(&mut s, outcome).unwrap());
    let node = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&SHADER).unwrap();
    let out = node.history.iter().find(|g| Some(g.id) == node.active).unwrap();
    let Some(Value::Media(result)) = out.outputs.first() else { panic!() };
    let (_, image) = nodecraft_media::decode_image(s.assets().get(&result.hash).unwrap(), &nodecraft_media::Limits::default()).unwrap();
    assert_eq!(image.to_rgba8().get_pixel(0, 0).0, [255, 175, 95, 255], "inverted");
}

#[test]
fn kept_values_move_into_the_new_range_and_damaged_config_is_an_error_not_a_panic() {
    let mut s = session();
    s.dispatch(
        "node.param_set",
        &json!({"canvas": CANVAS, "id": SHADER, "name": "amount", "value": Value::Number(nodecraft_types::Number::new(1.0).unwrap())}),
    )
    .unwrap();
    let source = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color * amount(); }";
    s.dispatch(
        "node.shader_set",
        &json!({"canvas": CANVAS, "id": SHADER, "source": source, "params": [{"name": "amount", "label": "Amount", "min": 0.0, "max": 0.5, "default": 0.25}]}),
    )
    .unwrap();
    let node = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&SHADER).unwrap().clone();
    assert_eq!(node.params.get("amount"), Some(&Value::Number(nodecraft_types::Number::new(0.5).unwrap())));
    s.kinds().get("image.shader").unwrap().validate_instance(&node, true).expect("still plannable");
    let kind = s.kinds().get("image.shader").unwrap();
    for bad in
        [Value::Bool(true), Value::Text("{not json".into()), Value::Text("x".repeat(70 * 1024)), Value::Text(r#"{"source":"","params":[],"extra":1}"#.into())]
    {
        let mut damaged = node.clone();
        damaged.config.insert(nodecraft_nodes::shader::CONFIG.into(), bad);
        assert!(kind.resolve_instance(&damaged).is_err());
    }
    let mut inverted = node.clone();
    inverted.config.insert(
        nodecraft_nodes::shader::CONFIG.into(),
        Value::Text(r#"{"source":"","params":[{"name":"a","label":"A","min":1.0,"max":0.0,"default":0.5}]}"#.into()),
    );
    assert!(kind.resolve_instance(&inverted).is_err(), "min above max");
}

#[test]
fn escaped_shader_over_reader_limit_preserves_document_and_history() {
    let mut s = session();
    let initial = s.document().clone();
    s.dispatch(
        "node.shader_set",
        &json!({"canvas": CANVAS, "id": SHADER, "source": POSTERIZE,
            "params": [{"name": "levels", "label": "Levels", "min": 2.0, "max": 16.0, "default": 4.0}]}),
    )
    .unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let status = s.file_status();
    let history = (s.can_undo(), s.can_redo());
    let effect = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }";
    let source = "\t".repeat(nodecraft_gpu::shader::MAX_SOURCE - effect.len()) + effect;
    let params: Vec<_> = (0..16).map(|i| json!({"name": format!("p{i}"), "label": "P", "min": 0.0, "max": 1.0, "default": 0.0})).collect();
    let result = s.dispatch("node.shader_set", &json!({"canvas": CANVAS, "id": SHADER, "source": source, "params": params}));
    assert!(result.is_err(), "an encoded shader above its reader limit must be refused, got {result:?}");
    let error = result.unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::InvalidInput);
    assert!(error.message.contains("64 KiB"), "{}", error.message);
    assert_eq!(s.document(), &before);
    assert_eq!(s.revision(), revision);
    assert_eq!(s.file_status(), status);
    assert_eq!((s.can_undo(), s.can_redo()), history);
    assert!(s.events_since(revision).unwrap().is_empty());
    assert_eq!(sliders(&s), ["levels"], "the previous shader stays readable");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &initial, "refusal added no undo entry");
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.document(), &before, "the previous successful edit remains redoable");
}
