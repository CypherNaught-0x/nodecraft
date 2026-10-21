use super::*;
use nodecraft_nodes::{Category, Registry};
use nodecraft_types::NodeId;

const OBJ: &[u8] = b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
const PAGE: &str = "BT /F1 12 Tf 20 150 Td (Hello) Tj ET";

/// A PDF of `pages` in `assets`, as a source holds it.
fn pdf_value(assets: &mut AssetStore, pages: &[&str]) -> Value {
    let hash = assets.insert(&nodecraft_media::pdf_fixture::pdf(pages)).unwrap();
    Value::Media(AssetRef { hash, mime: "application/pdf".parse().unwrap(), meta: MediaMeta::default() })
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(width, height, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

/// A kind's default visible parameters.
fn defaults(kinds: &Registry, id: &str) -> ParamValues {
    let kind = kinds.get(id).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
    let mut params = kind.validate_instance(&node, true).unwrap().into_visible_values();
    // The mask's strokes travel as a hidden parameter, added when the run is planned.
    if id == nodecraft_nodes::mask::ID {
        params.insert(nodecraft_nodes::mask::SPEC_PARAM.into(), Value::Text(nodecraft_nodes::mask::stored(&node).unwrap().into()));
    }
    params
}

#[test]
fn every_action_kind_runs_with_its_defaults() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png(8, 6)).unwrap();
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let model = Value::Media(AssetRef { hash: assets.insert(OBJ).unwrap(), mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
    let document = pdf_value(&mut assets, &[PAGE]);
    let actions = LocalActions::new(assets);
    let mut ran = 0;
    // The custom shader and 3D captures need a GPU, and the timeline and video actions ffmpeg:
    // their own tests run them. An Element plans source jobs, not an action.
    let skipped = ["image.shader", nodecraft_nodes::mesh::PHOTO, nodecraft_nodes::mesh::VIEWS, nodecraft_nodes::element::ID, nodecraft_nodes::timeline::ID];
    let runs = |id: &str| !skipped.contains(&id) && !nodecraft_nodes::video_edit::IDS.contains(&id) && !nodecraft_nodes::video_frames::IDS.contains(&id);
    for descriptor in kinds.descriptors().filter(|d| d.category == Category::Action && runs(&d.id)) {
        let input = if descriptor.id.starts_with("image.") {
            image.clone()
        } else if descriptor.id.starts_with("mesh.") {
            model.clone()
        } else if descriptor.id.starts_with("document.") {
            document.clone()
        } else {
            Value::Text("a, b\\nc".into())
        };
        let inputs = vec![input; descriptor.inputs.len()];
        let out = actions.execute(&descriptor.id, &defaults(&kinds, &descriptor.id), &inputs).unwrap_or_else(|e| panic!("{}: {e}", descriptor.id));
        if descriptor.id.starts_with("mesh.") {
            assert!(matches!(&out, Value::Media(asset) if asset.meta.triangles == Some(2)), "{}: {out:?}", descriptor.id);
            ran += 1;
            continue;
        }
        match (&out, descriptor.id.starts_with("image.") || descriptor.id == nodecraft_nodes::document::PAGES) {
            (Value::Media(asset), true) => assert!(asset.meta.width.is_some(), "{}", descriptor.id),
            (Value::List(items), true) => assert!(items.iter().all(|i| matches!(i, Value::Media(_))), "{}", descriptor.id),
            (Value::Text(_) | Value::List(_), false) => {}
            other => panic!("{}: {other:?}", descriptor.id),
        }
        ran += 1;
    }
    assert_eq!(ran, 23);
    // Every new image and model is staged for the host to import, and nothing else.
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
    for descriptor in kinds.descriptors().filter(|d| d.category == Category::Action && d.id.starts_with("image.") && d.id != "image.shader") {
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
                let inputs = vec![image.clone(); descriptor.inputs.len()];
                actions.execute(&descriptor.id, &params, &inputs).unwrap_or_else(|e| panic!("{} {}={value:?}: {e}", descriptor.id, spec.name));
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

#[test]
fn the_new_kinds_do_what_they_say() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let base = assets.insert(&png(40, 30)).unwrap();
    let mark = assets.insert(&png(10, 10)).unwrap();
    let media = |hash| Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let Value::Media(over) = actions.execute("image.overlay", &defaults(&kinds, "image.overlay"), &[media(base), media(mark)]).unwrap() else { panic!() };
    assert_eq!((over.meta.width, over.meta.height), (Some(40), Some(30)), "the base image's size");
    let mut three = defaults(&kinds, "image.duplicate");
    three.insert("count".into(), Value::Unsigned(nodecraft_types::Unsigned(3)));
    let Value::List(copies) = actions.execute("image.duplicate", &three, &[media(base)]).unwrap() else { panic!() };
    assert_eq!(copies.len(), 3);
    assert!(copies.windows(2).all(|w| w[0] == w[1]), "one image, referred to three times");
    let mut many = defaults(&kinds, "image.duplicate");
    many.insert("count".into(), Value::Unsigned(nodecraft_types::Unsigned(64)));
    let Value::List(all) = actions.execute("image.duplicate", &many, &[media(base)]).unwrap() else { panic!() };
    assert_eq!(all.len(), 64);
    let Value::Media(square) = actions.execute("image.aspect", &defaults(&kinds, "image.aspect"), &[media(base)]).unwrap() else { panic!() };
    assert_eq!(square.meta.width, square.meta.height, "square by default");
    let mut template = defaults(&kinds, "text.template");
    template.insert("template".into(), Value::Text("{{2}} then {{1}}".into()));
    assert_eq!(
        actions.execute("text.template", &template, &[Value::List(vec![Value::Text("a".into()), Value::Text("b".into())])]).unwrap(),
        Value::Text("b then a".into())
    );
    template.insert("template".into(), Value::Text("{{3}}".into()));
    assert!(actions.execute("text.template", &template, &[Value::Text("only one".into())]).is_err(), "a field nothing fills");
    // Overlay without its second image says so.
    assert!(actions.execute("image.overlay", &defaults(&kinds, "image.overlay"), &[media(base)]).is_err());
}

#[test]
fn the_layer_editor_draws_its_stored_layers_over_a_fitted_image() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let hash = assets.insert(&png(200, 100)).unwrap();
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let mut params = defaults(&kinds, nodecraft_nodes::layers::ID);
    params.insert("width".into(), Value::Unsigned(nodecraft_types::Unsigned(300)));
    params.insert("height".into(), Value::Unsigned(nodecraft_types::Unsigned(300)));
    let shape =
        r##"{"layers":[{"id":"box","name":"Box","x":150,"y":150,"width":20,"height":20,"content":{"kind":"shape","shape":"rectangle","fill":"#ff0000"}}]}"##;
    params.insert(nodecraft_nodes::layers::SPEC_PARAM.into(), Value::Text(shape.into()));
    params.insert(nodecraft_nodes::layers::SOURCES_PARAM.into(), Value::List(vec![Value::Text("a".into())]));
    let Value::Media(out) = actions.execute(nodecraft_nodes::layers::ID, &params, &[image]).unwrap() else { panic!("an image") };
    assert_eq!((out.meta.width, out.meta.height), (Some(300), Some(300)));
    let staged = actions.take_staged();
    let png = staged.get(&out.hash).unwrap();
    let (_, decoded) = nodecraft_media::decode_image(png, &nodecraft_media::Limits::default()).unwrap();
    let rgba = decoded.to_rgba8();
    let px = |x: u32, y: u32| rgba.get_pixel(x, y).0.to_vec();
    // The image fits the frame's width and is centred, so the white background shows above it,
    // and the image layer, added on top, covers the stored shape.
    assert_eq!(px(150, 10), vec![255, 255, 255, 255]);
    assert_eq!(px(150, 150)[1..], [80, 160, 255]);
    // A hostile spec is an error, not a panic.
    params.insert(nodecraft_nodes::layers::SPEC_PARAM.into(), Value::Text("{".into()));
    assert!(actions.execute(nodecraft_nodes::layers::ID, &params, &[]).is_err());
}

#[test]
fn an_overlay_plans_with_both_of_its_inputs_in_port_order() {
    use nodecraft_nodes::{InputValue, PlanCtx};
    use nodecraft_types::{Modality, PortType};
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get("image.overlay").unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
    let t = PortType { modality: Modality::Image, many: false };
    let input = |port: &str, n: u8| InputValue { port: port.into(), port_type: t, value: Value::Text(format!("{n}")), origins: vec![] };
    // Connected in the "wrong" order: planning still puts the base image first.
    let jobs =
        kind.plan(&PlanCtx { node: &node, inputs: &[input("overlay", 2), input("image", 1)], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
    let Some(nodecraft_nodes::JobSpec::Action { inputs, .. }) = jobs.first() else { panic!() };
    assert_eq!(inputs, &vec![Value::Text("1".into()), Value::Text("2".into())]);
    assert!(kind.plan(&PlanCtx { node: &node, inputs: &[input("image", 1)], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err(), "both are needed");
    assert!(kind.mode(&[t, t]).is_some() && kind.mode(&[t]).is_some() && kind.mode(&[t, t, t]).is_none());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_imported_image_runs_local_actions_without_resident_materialization() {
    use nodecraft_format::{
        NativeStorage, ProjectStorage,
        asset_source::{AssetRange, MemorySource},
    };
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let mut assets = AssetStore::with_context(storage.asset_context().unwrap());
    let bytes = png(40, 20);
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes.as_slice()))), 0, bytes.len() as u64).unwrap();
    let hash = assets.import_stream(&range, &Default::default(), &Default::default()).unwrap();
    assert!(assets.get(&hash).is_none());
    let actions = LocalActions::new(assets);
    let kinds = Registry::builtins().unwrap();
    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let Value::Media(result) = actions.execute("image.resize", &defaults(&kinds, "image.resize"), &[image]).unwrap() else { panic!() };
    assert_eq!((result.meta.width, result.meta.height), (Some(20), Some(10)));
}

#[test]
fn convert_3d_writes_each_format_and_chains_from_staged_results() {
    let mut assets = AssetStore::default();
    let hash = assets.insert(OBJ).unwrap();
    let model = Value::Media(AssetRef { hash, mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let format = |f: &str| ParamValues::from([("format".into(), Value::Text(f.into()))]);
    let glb = actions.execute(nodecraft_nodes::mesh::CONVERT, &format("glb"), std::slice::from_ref(&model)).unwrap();
    let Value::Media(glb_asset) = &glb else { panic!("a model") };
    assert_eq!((glb_asset.mime.essence(), glb_asset.meta.vertices, glb_asset.meta.triangles), ("model/gltf-binary", Some(4), Some(2)));
    // The GLB was staged, so a second conversion in the same run reads it.
    let obj = actions.execute(nodecraft_nodes::mesh::CONVERT, &format("obj"), &[glb]).unwrap();
    let Value::Media(obj_asset) = &obj else { panic!("a model") };
    assert_eq!(obj_asset.mime.essence(), "model/obj");
    let staged = actions.take_staged();
    let written = nodecraft_media::decode_mesh(staged.get(&obj_asset.hash).unwrap(), &Default::default()).unwrap();
    assert_eq!(written.triangle_count(), 2);
    assert!(actions.execute(nodecraft_nodes::mesh::CONVERT, &format("stl"), std::slice::from_ref(&model)).unwrap_err().contains("stl"));
    assert!(actions.execute(nodecraft_nodes::mesh::CONVERT, &format("glb"), &[Value::Text("x".into())]).unwrap_err().contains("connect a 3D model"));
    let mut broken = AssetStore::default();
    let junk = Value::Media(AssetRef { hash: broken.insert(b"v 0 0\n").unwrap(), mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
    assert!(LocalActions::new(broken).execute(nodecraft_nodes::mesh::CONVERT, &format("glb"), &[junk]).unwrap_err().contains("can't be read"));
}

/// A capture's result, or `None` on a machine with no GPU.
fn capture(actions: &LocalActions, action: &str, params: &ParamValues, model: &Value) -> Option<AssetRef> {
    match actions.execute(action, params, std::slice::from_ref(model)) {
        Ok(Value::Media(asset)) => Some(asset),
        Err(e) if e.contains("need a GPU") => None,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_3d_photo_is_an_image_of_its_size_and_repeats_exactly() {
    let mut assets = AssetStore::default();
    let model = Value::Media(AssetRef { hash: assets.insert(OBJ).unwrap(), mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let kinds = Registry::builtins().unwrap();
    let mut params = defaults(&kinds, nodecraft_nodes::mesh::PHOTO);
    params.insert("width".into(), Value::Unsigned(nodecraft_types::Unsigned(64)));
    params.insert("height".into(), Value::Unsigned(nodecraft_types::Unsigned(48)));
    let Some(photo) = capture(&actions, nodecraft_nodes::mesh::PHOTO, &params, &model) else { return };
    assert_eq!((photo.mime.essence(), photo.meta.width, photo.meta.height), ("image/png", Some(64), Some(48)));
    assert_eq!(capture(&actions, nodecraft_nodes::mesh::PHOTO, &params, &model).unwrap().hash, photo.hash, "the same settings draw the same image");
    params.insert("yaw".into(), Value::Number(nodecraft_types::Number::new(120.0).unwrap()));
    assert_ne!(capture(&actions, nodecraft_nodes::mesh::PHOTO, &params, &model).unwrap().hash, photo.hash);
    // The default background is transparent; the model covers the middle.
    let staged = actions.take_staged();
    let (_, image) = nodecraft_media::decode_image(staged.get(&photo.hash).unwrap(), &Default::default()).unwrap();
    let rgba = image.into_rgba8();
    assert_eq!(rgba.get_pixel(0, 0).0[3], 0);
    assert_eq!(rgba.get_pixel(32, 24).0[3], 255);
    assert!(actions.execute(nodecraft_nodes::mesh::PHOTO, &params, &[Value::Text("x".into())]).unwrap_err().contains("connect a 3D model"));
    params.insert("style".into(), Value::Text("clay".into()));
    assert!(actions.execute(nodecraft_nodes::mesh::PHOTO, &params, std::slice::from_ref(&model)).unwrap_err().contains("clay"));
}

#[test]
fn a_view_sheet_lays_its_views_out_in_a_grid_and_turns_either_way() {
    // A right triangle: its mirror image (the view from behind) differs from it.
    let triangle = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
    let mut assets = AssetStore::default();
    let model = Value::Media(AssetRef { hash: assets.insert(triangle).unwrap(), mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let kinds = Registry::builtins().unwrap();
    let mut params = defaults(&kinds, nodecraft_nodes::mesh::VIEWS);
    params.insert("size".into(), Value::Unsigned(nodecraft_types::Unsigned(64)));
    for (count, size) in [("4", (128, 128)), ("8", (256, 128)), ("16", (256, 256))] {
        params.insert("count".into(), Value::Text(count.into()));
        let Some(sheet) = capture(&actions, nodecraft_nodes::mesh::VIEWS, &params, &model) else { return };
        assert_eq!((sheet.meta.width, sheet.meta.height), (Some(size.0), Some(size.1)), "{count} views");
    }
    params.insert("count".into(), Value::Text("4".into()));
    params.insert("start".into(), Value::Number(nodecraft_types::Number::new(30.0).unwrap()));
    let ccw = capture(&actions, nodecraft_nodes::mesh::VIEWS, &params, &model).unwrap();
    params.insert("direction".into(), Value::Text("cw".into()));
    let cw = capture(&actions, nodecraft_nodes::mesh::VIEWS, &params, &model).unwrap();
    assert_ne!(ccw.hash, cw.hash);
    params.insert("count".into(), Value::Text("5".into()));
    assert!(actions.execute(nodecraft_nodes::mesh::VIEWS, &params, std::slice::from_ref(&model)).unwrap_err().contains("4, 8 or 16"));
}

#[test]
fn each_tile_of_a_view_sheet_is_the_photo_from_its_angle_in_reading_order() {
    // A right triangle looks different from every quarter turn.
    let triangle = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 2 3\nf 1 3 4\n";
    let mut assets = AssetStore::default();
    let model = Value::Media(AssetRef { hash: assets.insert(triangle).unwrap(), mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    let kinds = Registry::builtins().unwrap();
    let side = 48u32;
    let rgba = |asset: &AssetRef| {
        let bytes = actions.staged().get(&asset.hash).unwrap().to_vec();
        nodecraft_media::decode_image(&bytes, &nodecraft_media::Limits::default()).unwrap().1.to_rgba8().into_raw()
    };
    for (direction, turn) in [("ccw", 1.0), ("cw", -1.0)] {
        let mut sheet = defaults(&kinds, nodecraft_nodes::mesh::VIEWS);
        sheet.insert("count".into(), Value::Text("4".into()));
        sheet.insert("start".into(), Value::Number(nodecraft_types::Number::new(30.0).unwrap()));
        sheet.insert("direction".into(), Value::Text(direction.into()));
        sheet.insert("size".into(), Value::Unsigned(nodecraft_types::Unsigned(u64::from(side))));
        let Some(sheet) = capture(&actions, nodecraft_nodes::mesh::VIEWS, &sheet, &model) else { return };
        let sheet = rgba(&sheet);
        let row = side as usize * 4;
        for index in 0..4usize {
            let mut photo = defaults(&kinds, nodecraft_nodes::mesh::PHOTO);
            let yaw = 30.0 + turn * 90.0 * index as f64;
            photo.insert("yaw".into(), Value::Number(nodecraft_types::Number::new(yaw).unwrap()));
            photo.insert("width".into(), Value::Unsigned(nodecraft_types::Unsigned(u64::from(side))));
            photo.insert("height".into(), Value::Unsigned(nodecraft_types::Unsigned(u64::from(side))));
            let photo = rgba(&capture(&actions, nodecraft_nodes::mesh::PHOTO, &photo, &model).unwrap());
            // Tile `index` of a 2 × 2 sheet: left to right, then top to bottom.
            let (x, y) = (index % 2, index / 2);
            let tile: Vec<u8> = (0..side as usize).flat_map(|line| sheet[(y * side as usize + line) * row * 2 + x * row..][..row].iter().copied()).collect();
            // Angles reach the GPU by slightly different float paths; allow a few edge pixels.
            let differing = tile.iter().zip(&photo).filter(|(a, b)| a.abs_diff(**b) > 8).count();
            assert!(differing * 100 < tile.len(), "{direction} tile {index}: {differing} bytes differ");
        }
    }
}

#[test]
fn pdf_to_images_draws_each_page_in_the_range_at_its_size() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let document = pdf_value(&mut assets, &[PAGE, "", PAGE]);
    let actions = LocalActions::new(assets);
    let mut params = defaults(&kinds, nodecraft_nodes::document::PAGES);
    params.insert("size".into(), Value::Unsigned(nodecraft_types::Unsigned(150)));
    let Value::List(pages) = actions.execute(nodecraft_nodes::document::PAGES, &params, std::slice::from_ref(&document)).unwrap() else { panic!("a list") };
    assert_eq!(pages.len(), 3);
    for page in &pages {
        let Value::Media(asset) = page else { panic!("an image") };
        // The fixture's pages are 300 × 200 points.
        assert_eq!((asset.mime.essence(), asset.meta.width, asset.meta.height), ("image/png", Some(150), Some(100)));
    }
    params.insert("first".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    params.insert("last".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    let Value::List(one) = actions.execute(nodecraft_nodes::document::PAGES, &params, std::slice::from_ref(&document)).unwrap() else { panic!("a list") };
    assert_eq!(one.len(), 1);
    // The staged pages are PNGs the host imports.
    let staged = actions.take_staged();
    let Some(Value::Media(asset)) = one.first() else { panic!("an image") };
    assert!(nodecraft_media::probe_image(staged.get(&asset.hash).unwrap(), &Default::default()).is_ok());
    params.insert("first".into(), Value::Unsigned(nodecraft_types::Unsigned(4)));
    params.insert("last".into(), Value::Unsigned(nodecraft_types::Unsigned(0)));
    assert!(actions.execute(nodecraft_nodes::document::PAGES, &params, std::slice::from_ref(&document)).unwrap_err().contains("has 3 pages"));
}

#[test]
fn pdf_to_images_can_draw_only_the_pages_without_text() {
    // A scan draws no text: only those pages go on to a vision model (#592).
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let document = pdf_value(&mut assets, &[PAGE, "", PAGE, ""]);
    let actions = LocalActions::new(assets);
    let mut params = defaults(&kinds, nodecraft_nodes::document::PAGES);
    assert_eq!(params.get("only"), Some(&Value::Text("all".into())), "every page by default, as before");
    params.insert("size".into(), Value::Unsigned(nodecraft_types::Unsigned(64)));
    params.insert("only".into(), Value::Text("without_text".into()));
    let run = |params: &ParamValues| actions.execute(nodecraft_nodes::document::PAGES, params, std::slice::from_ref(&document));
    let Value::List(scans) = run(&params).unwrap() else { panic!("a list") };
    assert_eq!(scans.len(), 2);
    params.insert("first".into(), Value::Unsigned(nodecraft_types::Unsigned(1)));
    params.insert("last".into(), Value::Unsigned(nodecraft_types::Unsigned(1)));
    assert_eq!(run(&params).unwrap(), Value::List(vec![]), "a range with no scans draws nothing");
    params.insert("only".into(), Value::Text("odd".into()));
    assert!(run(&params).is_err());
}

#[test]
fn pdf_to_images_caps_the_scans_it_draws() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let blank = vec![""; usize::try_from(nodecraft_nodes::document::MAX_IMAGE_PAGES).unwrap() + 1];
    let document = pdf_value(&mut assets, &blank);
    let actions = LocalActions::new(assets);
    let mut params = defaults(&kinds, nodecraft_nodes::document::PAGES);
    params.insert("size".into(), Value::Unsigned(nodecraft_types::Unsigned(64)));
    params.insert("only".into(), Value::Text("without_text".into()));
    let err = actions.execute(nodecraft_nodes::document::PAGES, &params, std::slice::from_ref(&document)).unwrap_err();
    assert!(err.contains("narrow the page range"), "{err}");
    assert_eq!(actions.take_staged().len(), 0, "the scans are counted before any is drawn");
}

#[test]
fn pdf_to_text_reads_the_range_with_the_chosen_separator() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let document = pdf_value(&mut assets, &["", PAGE, "", "BT /F1 12 Tf 20 150 Td (Two) Tj ET", "", "BT /F1 12 Tf 20 150 Td (Three) Tj ET", ""]);
    let actions = LocalActions::new(assets);
    let mut params = defaults(&kinds, nodecraft_nodes::document::TEXT);
    let run = |params: &ParamValues| actions.execute(nodecraft_nodes::document::TEXT, params, std::slice::from_ref(&document));
    assert_eq!(run(&params).unwrap(), Value::Text("Hello\n\nTwo\n\nThree".into()), "pages without text leave no gap");
    params.insert("first".into(), Value::Unsigned(nodecraft_types::Unsigned(4)));
    params.insert("separator".into(), Value::Text("heading".into()));
    // Headings list every page, and mark the ones that drew no text (#592).
    assert_eq!(run(&params).unwrap(), Value::Text("[Page 4]\nTwo\n\n[Page 5: no text read]\n\n[Page 6]\nThree\n\n[Page 7: no text read]".into()));
    params.insert("first".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    params.insert("last".into(), Value::Unsigned(nodecraft_types::Unsigned(1)));
    assert!(run(&params).unwrap_err().contains("comes before"));
    params.insert("separator".into(), Value::Text("nope".into()));
    assert!(run(&params).is_err());
}

#[test]
fn a_document_action_without_a_readable_pdf_is_a_message() {
    let kinds = Registry::builtins().unwrap();
    let mut assets = AssetStore::default();
    let junk =
        Value::Media(AssetRef { hash: assets.insert(b"%PDF-1.7 not really").unwrap(), mime: "application/pdf".parse().unwrap(), meta: MediaMeta::default() });
    let image = Value::Media(AssetRef { hash: assets.insert(&png(4, 4)).unwrap(), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    let actions = LocalActions::new(assets);
    for action in [nodecraft_nodes::document::PAGES, nodecraft_nodes::document::TEXT] {
        let params = defaults(&kinds, action);
        assert!(actions.execute(action, &params, &[]).unwrap_err().contains("connect a PDF"), "{action}");
        assert!(actions.execute(action, &params, &[Value::Text("x".into())]).unwrap_err().contains("connect a PDF"), "{action}");
        assert!(actions.execute(action, &params, std::slice::from_ref(&junk)).unwrap_err().contains("can't be read"), "{action}");
        assert!(actions.execute(action, &params, std::slice::from_ref(&image)).unwrap_err().contains("can't be read"), "{action}");
        assert!(actions.execute(action, &ParamValues::new(), std::slice::from_ref(&junk)).is_err(), "{action}");
    }
}
