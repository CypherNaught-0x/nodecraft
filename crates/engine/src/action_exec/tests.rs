use super::*;
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::NodeId;

fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(width, height, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

/// A kind's default visible parameters.
fn defaults(kinds: &Registry, id: &str) -> ParamValues {
    let kind = kinds.get(id).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
    kind.validate_instance(&node, true).unwrap().into_visible_values()
}

#[test]
fn every_action_kind_runs_with_its_defaults() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png(8, 6)).unwrap();
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let mut ran = 0;
    for descriptor in kinds.descriptors().filter(|d| d.category == Category::Action) {
        let input = if descriptor.id.starts_with("image.") { image.clone() } else { Value::Text("a, b\\nc".into()) };
        let out = actions.execute(&descriptor.id, &defaults(&kinds, &descriptor.id), &[input]).unwrap_or_else(|e| panic!("{}: {e}", descriptor.id));
        match (&out, descriptor.id.starts_with("image.")) {
            (Value::Media(asset), true) => assert!(asset.meta.width.is_some(), "{}", descriptor.id),
            (Value::Text(_) | Value::List(_), false) => {}
            other => panic!("{}: {other:?}", descriptor.id),
        }
        ran += 1;
    }
    assert_eq!(ran, 11);
    // Every new image is staged for the host to import, and nothing else.
    let staged = actions.take_staged();
    assert!(!staged.is_empty());
    assert_eq!(actions.take_staged().len(), 0);
}

#[test]
fn settings_shape_the_result() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png(40, 20)).unwrap();
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let Value::Media(half) = actions.execute("image.resize", &defaults(&kinds, "image.resize"), std::slice::from_ref(&image)).unwrap() else { panic!() };
    assert_eq!((half.meta.width, half.meta.height), (Some(20), Some(10)));
    let mut params = defaults(&kinds, "text.split");
    params.insert("separator".into(), Value::Text(";".into()));
    assert_eq!(
        actions.execute("text.split", &params, &[Value::Text("a;b;c".into())]).unwrap(),
        Value::List(vec![Value::Text("a".into()), Value::Text("b".into()), Value::Text("c".into())])
    );
    let mut join = defaults(&kinds, "text.concat");
    join.insert("separator".into(), Value::Text("+".into()));
    assert_eq!(
        actions.execute("text.concat", &join, &[Value::Text("a".into()), Value::List(vec![Value::Text("b".into())])]).unwrap(),
        Value::Text("a+b".into())
    );
    let mut replace = defaults(&kinds, "text.replace");
    replace.insert("find".into(), Value::Text("cat".into()));
    replace.insert("replacement".into(), Value::Text("dog".into()));
    assert_eq!(actions.execute("text.replace", &replace, &[Value::Text("a cat".into())]).unwrap(), Value::Text("a dog".into()));
}

#[test]
fn bad_inputs_are_messages_not_panics() {
    let kinds = Registry::builtins().unwrap();
    let actions = LocalActions::new(AssetStore::default());
    let missing = Value::Media(AssetRef { hash: nodecraft_types::Blake3::from_bytes([3; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    assert!(actions.execute("image.blur", &defaults(&kinds, "image.blur"), &[missing]).unwrap_err().contains("isn't in this project"));
    assert!(actions.execute("image.blur", &defaults(&kinds, "image.blur"), &[Value::Text("x".into())]).is_err());
    assert!(actions.execute("image.blur", &ParamValues::new(), &[]).is_err());
    assert!(actions.execute("text.split", &defaults(&kinds, "text.split"), &[]).is_err());
    assert!(actions.execute("nope.thing", &ParamValues::new(), &[]).is_err());
    let mut bad = defaults(&kinds, "image.tint");
    bad.insert("color".into(), Value::Text("#zzzzzz".into()));
    assert!(actions.execute("image.tint", &bad, &[]).is_err());
}

#[test]
fn an_image_wired_into_blur_plans_and_runs_to_a_new_asset() {
    use nodecraft_exec::planner::{Planned, RunMode, plan};
    use nodecraft_exec::run::{Run, RunOptions};
    use nodecraft_exec::scheduler::RetryPolicy;
    use serde_json::json;
    let canvas = nodecraft_types::CanvasId::from_bytes([1; 16]);
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "C") }.apply(&mut project).unwrap();
    let mut session = crate::Session::new(project).unwrap();
    let hash = session.import_asset(&png(16, 12)).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(16), height: Some(12), ..MediaMeta::default() } };
    let (source, blur) = (NodeId::from_bytes([10; 16]), NodeId::from_bytes([11; 16]));
    session.dispatch("node.create", &json!({"canvas": canvas, "id": source, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": source, "name": "asset", "value": Value::Media(asset)})).unwrap();
    session.dispatch("node.create", &json!({"canvas": canvas, "id": blur, "kind": "image.blur", "pos": {"x": 400, "y": 0}})).unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([12; 16]);
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas": canvas, "edge": {"id": edge, "from": {"node": source, "port": "out"}, "to": {"node": blur, "port": "image"}, "order": 0}}),
        )
        .unwrap();
    let kinds = session.kinds();
    let live = session.document().canvases().get(&canvas).unwrap().clone();
    let stale = nodecraft_exec::staleness::stale(&live, kinds).unwrap();
    let Planned::Ready(planned) = plan(&live, kinds, &RunMode::WithUpstream, &[blur].into_iter().collect(), &stale).unwrap() else { panic!("blocked") };
    let options = RunOptions { id: [7; 16], first_job: 1, limits: Default::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(&planned, canvas, &live, kinds, options).unwrap();
    let actions = LocalActions::new(session.assets().clone());
    let mut queue: std::collections::VecDeque<_> = first.jobs.into_iter().collect();
    let mut finished = first.finished;
    while let Some(job) = queue.pop_front() {
        let progress = match run.work(job.id).cloned().unwrap() {
            nodecraft_nodes::JobSpec::Source { output, value, .. } => run.job_done(kinds, job.id, vec![(output, value)]),
            nodecraft_nodes::JobSpec::Action { action, params, inputs, output, .. } => {
                let value = actions.execute(&action, &params, &inputs).unwrap();
                run.job_done(kinds, job.id, vec![(output, value)])
            }
            other => panic!("unexpected {other:?}"),
        };
        assert!(progress.failed.is_empty(), "{:?}", progress.failed);
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
    }
    let result = finished.iter().find(|r| r.node == blur).expect("the blur ran");
    let Some(Value::Media(out)) = result.generation.outputs.first() else { panic!("an image") };
    assert_ne!(out.hash, hash, "a new image");
    // The host imports what the action made before recording.
    let staged = actions.take_staged();
    let bytes = staged.get(&out.hash).unwrap().to_vec();
    assert_eq!(session.import_asset(&bytes).unwrap(), out.hash);
}

#[test]
fn every_action_kind_round_trips_its_parameters_through_undo() {
    use serde_json::json;
    let canvas = nodecraft_types::CanvasId::from_bytes([1; 16]);
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "C") }.apply(&mut project).unwrap();
    let mut session = crate::Session::new(project).unwrap();
    let kinds: Vec<_> = session.kinds().descriptors().filter(|d| d.category == Category::Action).cloned().collect();
    for (i, descriptor) in kinds.iter().enumerate() {
        let id = NodeId::from_bytes([20 + i as u8; 16]);
        session.dispatch("node.create", &json!({"canvas": canvas, "id": id, "kind": descriptor.id, "pos": {"x": 0, "y": 0}})).unwrap();
        for spec in &descriptor.params.params {
            let Some(default) = spec.default.clone() else { continue };
            let changed = match (&spec.control, &default) {
                (nodecraft_types::ParamControl::Slider { max: Some(max), .. }, _) => Value::Number(*max),
                (nodecraft_types::ParamControl::Toggle, Value::Bool(b)) => Value::Bool(!b),
                (nodecraft_types::ParamControl::Choice { options }, _) => Value::Text(options.last().unwrap().value.clone()),
                (nodecraft_types::ParamControl::Unsigned { max: Some(max), .. }, _) => Value::Unsigned(*max),
                (nodecraft_types::ParamControl::Color, _) => Value::Text("#123456".into()),
                _ => Value::Text("x".into()),
            };
            let before = session.document().clone();
            session
                .dispatch("node.param_set", &json!({"canvas": canvas, "id": id, "name": spec.name, "value": changed}))
                .unwrap_or_else(|e| panic!("{} {}: {}", descriptor.id, spec.name, e.message));
            let after = session.document().clone();
            session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(session.document(), &before, "{} {}", descriptor.id, spec.name);
            session.dispatch("edit.redo", &json!({})).unwrap();
            assert_eq!(session.document(), &after, "{} {}", descriptor.id, spec.name);
        }
    }
}

#[test]
fn actions_chain_within_a_run() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png(40, 20)).unwrap();
    let actions = LocalActions::new(assets);
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let cropped = actions.execute("image.crop", &defaults(&kinds, "image.crop"), &[image]).unwrap();
    // The crop exists only in staging; the resize reads it from there.
    let Value::Media(out) = actions.execute("image.resize", &defaults(&kinds, "image.resize"), &[cropped]).unwrap() else { panic!() };
    assert_eq!((out.meta.width, out.meta.height), (Some(20), Some(10)));
}

#[test]
fn a_huge_image_is_refused_before_it_is_decoded() {
    let kinds = Registry::builtins().unwrap();
    // 4,097 × 4,096 transparent pixels compress to a small file but exceed the actions' 16.7 MP.
    let pixels = vec![0u8; 4097 * 4096 * 4];
    let big = nodecraft_media::encode_png(4097, 4096, &pixels, &nodecraft_media::Limits::default()).unwrap();
    assert!(big.len() < nodecraft_format::MAX_ASSET_BYTES);
    let mut assets = AssetStore::default();
    let hash = assets.insert(&big).unwrap();
    let actions = LocalActions::new(assets);
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    assert!(actions.execute("image.blur", &defaults(&kinds, "image.blur"), &[image]).unwrap_err().contains("megapixels"));
}

#[test]
fn every_choice_and_slider_end_runs() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png(12, 9)).unwrap();
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    for descriptor in kinds.descriptors().filter(|d| d.category == Category::Action && d.id.starts_with("image.")) {
        for spec in &descriptor.params.params {
            let values: Vec<Value> = match &spec.control {
                nodecraft_types::ParamControl::Slider { min: Some(min), max: Some(max), .. } => vec![Value::Number(*min), Value::Number(*max)],
                nodecraft_types::ParamControl::Choice { options } => options.iter().map(|o| Value::Text(o.value.clone())).collect(),
                nodecraft_types::ParamControl::Toggle => vec![Value::Bool(true), Value::Bool(false)],
                _ => continue,
            };
            for value in values {
                let mut params = defaults(&kinds, &descriptor.id);
                params.insert(spec.name.clone(), value.clone());
                actions
                    .execute(&descriptor.id, &params, std::slice::from_ref(&image))
                    .unwrap_or_else(|e| panic!("{} {}={value:?}: {e}", descriptor.id, spec.name));
            }
        }
    }
    // Crop corners anywhere in range stay inside the image.
    let mut corner = defaults(&kinds, "image.crop");
    corner.insert("x".into(), Value::Number(nodecraft_types::Number::new(0.9).unwrap()));
    corner.insert("y".into(), Value::Number(nodecraft_types::Number::new(0.5).unwrap()));
    actions.execute("image.crop", &corner, &[image]).unwrap();
    let mut split = defaults(&kinds, "text.split");
    split.insert("separator".into(), Value::Text(String::new()));
    assert!(actions.execute("text.split", &split, &[Value::Text("a".into())]).unwrap_err().contains("separator"));
}
