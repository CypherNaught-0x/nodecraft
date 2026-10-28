use super::*;
use nodecraft_types::{Number, Unsigned};
fn project() -> (tempfile::TempDir, PathBuf, NodeId) {
    let mut session = nodecraft_automation::blank_session(ProjectId::from_bytes([97; 16]), CanvasId::from_bytes([1; 16])).unwrap();
    let node = NodeId::from_bytes([2; 16]);
    session.dispatch("node.create", &json!({"canvas":CanvasId::from_bytes([1;16]),"id":node,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.ncraft");
    std::fs::write(&path, nodecraft_format::encode(&Snapshot { project: session.document().clone(), assets: AssetStore::default(), epoch: 0 }).unwrap())
        .unwrap();
    (dir, path, node)
}
#[test]
fn cli97_named_source_values_are_engine_mutations_in_an_isolated_read_only_snapshot() {
    let (_dir, path, node) = project();
    let before = std::fs::read(&path).unwrap();
    let target = Target::open(path.to_str().unwrap(), &Default::default()).unwrap();
    let raw = vec![format!("{node}=a=b,\nsecond line")];
    let bindings = target.bindings(&raw, Path::new("."), &mut Files::default()).unwrap();
    let mut session = target.session(Services::default()).unwrap();
    bindings[0].apply(&mut session, target.canvas).unwrap();
    assert_eq!(session.document().canvases()[&target.canvas].nodes()[&node].params["text"], Value::Text("a=b,\nsecond line".into()));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_ne!(
        target.session(Services::default()).unwrap().document().canvases()[&target.canvas].nodes()[&node].params.get("text"),
        Some(&Value::Text("a=b,\nsecond line".into()))
    );
}
#[test]
fn cli97_bad_and_duplicate_bindings_are_refused_before_provider_setup() {
    let (_dir, path, node) = project();
    let target = Target::open(path.to_str().unwrap(), &Default::default()).unwrap();
    for raw in [vec!["missing-separator".into()], vec!["unknown=value".into()], vec![format!("{node}=one"), format!("{node}.text=two")]] {
        assert!(target.bindings(&raw, Path::new("."), &mut Files::default()).is_err());
    }
}
#[test]
fn cli97_numeric_and_boolean_values_are_typed_without_loss_or_nonfinite_numbers() {
    assert_eq!(
        typed(&ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) }, "18446744073709551615").unwrap(),
        Value::Unsigned(Unsigned(u64::MAX))
    );
    assert!(typed(&ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) }, "18446744073709551616").is_err());
    let slider = ParamControl::Slider { min: None, max: None, step: Number::new(0.1).unwrap() };
    assert!(typed(&slider, "NaN").is_err());
    assert!(typed(&slider, "inf").is_err());
    assert_eq!(typed(&ParamControl::Toggle, "true").unwrap(), Value::Bool(true));
    assert!(typed(&ParamControl::Toggle, "yes").is_err());
}

#[test]
fn cli97_published_technique_form_binds_named_inputs_and_parameters_and_runs() {
    let (_dir, path, brief) = project();
    let mut source = crate::project::open_with_assets(&path).unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    let action = NodeId::from_bytes([3; 16]);
    source.dispatch("node.create", &json!({"canvas":canvas,"id":action,"kind":"text.replace","pos":{"x":300,"y":0}})).unwrap();
    source.dispatch("edge.connect",&json!({"canvas":canvas,"edge":{"id":nodecraft_types::EdgeId::from_bytes([4;16]),"from":{"node":brief,"port":"out"},"to":{"node":action,"port":"text"},"order":0}})).unwrap();
    source.dispatch("technique.publish",&json!({"canvas":canvas,"published":CanvasId::from_bytes([5;16]),"seed":NodeId::from_bytes([6;16]),"technique":nodecraft_types::TechniqueId::from_bytes([7;16]),"name":"Swap","inputs":[{"node":brief,"name":"Brief"}],"outputs":[{"node":action,"name":"Result"}],"params":[{"node":action,"param":"find","name":"Find"},{"node":action,"param":"replacement","name":"Replacement"}]})).unwrap();
    let original = nodecraft_format::encode(&Snapshot { project: source.document().clone(), assets: AssetStore::default(), epoch: 0 }).unwrap();
    std::fs::write(&path, &original).unwrap();
    let options = crate::Options { technique: Some("Swap".into()), ..Default::default() };
    let target = Target::open(path.to_str().unwrap(), &options).unwrap();
    let bindings = target.bindings(&["Brief=a cat".into(), "Find=cat".into(), "Replacement=dog".into()], Path::new("."), &mut Files::default()).unwrap();
    let mut services = Services::default();
    let mut runner = crate::run::Runner::new(&mut services, "cli97-form".into()).unwrap();
    let mut session = target.session(services).unwrap();
    for binding in bindings {
        binding.apply(&mut session, target.canvas).unwrap();
    }
    crate::quote::prepare(&mut session, target.canvas, &target.nodes, &mut |_| panic!("local technique does not need a provider")).unwrap();
    runner.install(&mut session);
    let quote = crate::quote::quote(&mut session, target.canvas, &target.nodes).unwrap();
    let steps = runner.run(&mut session, target.canvas, &target.nodes, &quote, crate::run::Consent { spend: false }).unwrap();
    assert!(steps.iter().all(|step| step["status"] == "recorded" || step["status"] == "reused"), "{steps:?}");
    let result = &session.document().canvases()[&target.canvas].nodes()[&target.nodes[0]];
    assert!(matches!(result.history.last().unwrap().outputs.as_slice(),[Value::Text(text)] if text=="a dog"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn cli97_media_paths_are_relative_to_the_csv_and_snapshotted_before_execution() {
    let (dir, path, _) = project();
    let mut source = crate::project::open_with_assets(&path).unwrap();
    let node = NodeId::from_bytes([3; 16]);
    let canvas = CanvasId::from_bytes([1; 16]);
    source.dispatch("node.create", &json!({"canvas":canvas,"id":node,"kind":"source.audio","pos":{"x":0,"y":0}})).unwrap();
    std::fs::write(&path, nodecraft_format::encode(&Snapshot { project: source.document().clone(), assets: AssetStore::default(), epoch: 0 }).unwrap())
        .unwrap();
    let wav = include_bytes!("../../../../crates/providers/src/fal/fixtures/tone.wav");
    std::fs::write(dir.path().join("tone.wav"), wav).unwrap();
    let target = Target::open(path.to_str().unwrap(), &Default::default()).unwrap();
    let mut files = Files::default();
    let first = target.bindings(&[format!("{node}=@tone.wav")], dir.path(), &mut files).unwrap();
    let second = target.bindings(&[format!("{node}=tone.wav")], dir.path(), &mut files).unwrap();
    let (Given::Media { bytes: a, .. }, Given::Media { bytes: b, .. }) = (&first[0].values[0], &second[0].values[0]) else { panic!("media") };
    assert!(Arc::ptr_eq(a, b), "rows share one frozen file body");
    std::fs::write(dir.path().join("tone.wav"), b"changed after preflight").unwrap();
    let mut session = target.session(Services::default()).unwrap();
    first[0].apply(&mut session, canvas).unwrap();
    let Value::Media(asset) = &session.document().canvases()[&canvas].nodes()[&node].params["asset"] else { panic!("audio") };
    let mut actual = Vec::new();
    session.assets().copy_verified_to(asset, &mut actual, &Default::default(), &Default::default()).unwrap();
    assert_eq!(actual, wav);
    assert!(target.bindings(&[format!("{node}=.")], dir.path(), &mut files).is_err());
}

#[test]
fn cli97_text_choice_and_model_controls_keep_literal_json_prefixes() {
    let choice: ParamControl = serde_json::from_value(json!({"type":"choice","options":[{"value":"json:literal","label":"Literal"}]})).unwrap();
    assert_eq!(typed(&choice, "json:literal").unwrap(), Value::Text("json:literal".into()));
    assert_eq!(typed(&ParamControl::Model, "json:literal").unwrap(), Value::Text("json:literal".into()));
}
