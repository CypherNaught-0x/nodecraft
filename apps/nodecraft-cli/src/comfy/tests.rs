use super::*;
const CATALOG: &str = r#"{
 "LoadImage":{"input":{"required":{"image":[["sample.png"],{"image_upload":true}]}},"output":["IMAGE","MASK"]},
 "SaveImage":{"input":{"required":{"images":["IMAGE",{}],"filename_prefix":["STRING",{}]}},"output":[],"output_node":true}
}"#;
const FLOW: &str = r#"{"1":{"class_type":"LoadImage","inputs":{"image":"sample.png"}},"2":{"class_type":"SaveImage","inputs":{"images":["1",0],"filename_prefix":"NodeCraft"}}}"#;
fn fetched() -> Result<workflow::Fetched, Error> {
    Ok(Arc::new(workflow::NodeCatalog::parse(CATALOG.as_bytes()).unwrap().0).into())
}
fn arguments(dir: &Path) -> ImportArgs {
    let workflow = dir.join("workflow.json");
    std::fs::write(&workflow, FLOW).unwrap();
    ImportArgs {
        workflow,
        out: dir.join("imported.ncraft"),
        project: None,
        canvas: None,
        server: Some("GPU".into()),
        title: Some("Comfy workflow".into()),
        params: Vec::new(),
        json: true,
    }
}
fn settings() -> ProviderSettings {
    ProviderSettings { comfyui: vec![nodecraft_engine::settings::ComfyServer::new("GPU", "http://127.0.0.1:8188")], ..Default::default() }
}
#[test]
fn cli97_comfy_import_uses_engine_commands_pins_selected_server_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let args = arguments(dir.path());
    let result = import(&args, &settings(), fetched).unwrap();
    let project = crate::project::open_with_assets(&args.out).unwrap();
    let canvas: CanvasId = result["canvas"].as_str().unwrap().parse().unwrap();
    let id: NodeId = result["node"].as_str().unwrap().parse().unwrap();
    let node = &project.document().canvases()[&canvas].nodes()[&id];
    assert_eq!(node.kind, "generation.comfy_workflow");
    assert_eq!(node.title, "Comfy workflow");
    assert_eq!(node.config[nodecraft_engine::provider_setup::COMFY_SERVER_CONFIG], nodecraft_types::Value::Text("GPU".into()));
    assert_eq!(result["import"]["ports"].as_array().unwrap().len(), 1);
    let original = std::fs::read(&args.out).unwrap();
    assert!(import(&args, &settings(), || panic!("existing output must be refused before network")).is_err());
    assert_eq!(std::fs::read(&args.out).unwrap(), original);
    assert_eq!(owned_entries(dir.path()), 2, "owned staging and native asset folders are retired");
}
#[test]
fn cli97_comfy_failed_fetch_or_conversion_publishes_no_project_and_preserves_input() {
    let dir = tempfile::tempdir().unwrap();
    let args = arguments(dir.path());
    assert!(import(&args, &settings(), || Err(invalid("fixture unavailable"))).is_err());
    assert!(!args.out.exists());
    std::fs::write(&args.workflow, r#"{"1":{"class_type":"MissingClass","inputs":{}}}"#).unwrap();
    assert!(import(&args, &settings(), fetched).is_err());
    assert!(!args.out.exists());
    assert_eq!(owned_entries(dir.path()), 1);
    for values in [vec!["flow.json", "--out", "a", "--out", "b"], vec!["flow.json", "--out"], vec!["flow.json", "--wat", "x"], vec!["flow.json"]] {
        assert!(import_args(&values.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
    }
}
#[test]
fn cli97_new_project_publication_preserves_large_indexed_assets_without_system_temp() {
    use nodecraft_types::{AssetRef, MediaMeta, Value as Typed};
    let dir = tempfile::tempdir().unwrap();
    let canvas = CanvasId::from_bytes([1; 16]);
    let mut project = Project::new(ProjectId::from_bytes([97; 16]), "Large CLI fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Video") }.apply(&mut project).unwrap();
    let context = crate::project::native_context(dir.path()).unwrap();
    let mut session =
        Session::with_storage(Snapshot { project, assets: AssetStore::with_context(context), epoch: 0 }, crate::project::ephemeral(dir.path()).unwrap())
            .unwrap();
    let mut movie = include_bytes!("../../../../crates/providers/src/fal/fixtures/blue.mp4").to_vec();
    let padding = 17 * 1024 * 1024u32;
    movie.extend_from_slice(&padding.to_be_bytes());
    movie.extend_from_slice(b"free");
    movie.resize(movie.len() + padding as usize - 8, 0);
    let hash = session.import_asset_reader(&mut std::io::Cursor::new(&movie), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let node = NodeId::from_bytes([2; 16]);
    session.dispatch("node.create", &json!({"canvas":canvas,"id":node,"kind":"source.video","pos":{"x":0,"y":0}})).unwrap();
    session.dispatch("node.param_set",&json!({"canvas":canvas,"id":node,"name":"asset","value":Typed::Media(AssetRef { hash,mime:"video/mp4".parse().unwrap(),meta:MediaMeta::default() })})).unwrap();
    let original = dir.path().join("large.ncraft");
    Destination::new(&original).unwrap().save(&session).unwrap();
    drop(session);
    let mut args = arguments(dir.path());
    args.project = Some(original.clone());
    let before = std::fs::read(&original).unwrap();
    import(&args, &settings(), fetched).unwrap();
    let context = crate::project::native_context(dir.path()).unwrap();
    let restored = crate::project::snapshot_in_context(&args.out, context).unwrap();
    let asset = &restored.project.canvases()[&canvas].nodes()[&node].params["asset"];
    let Typed::Media(asset) = asset else { panic!("media reference retained") };
    let mut copied = Vec::new();
    restored.assets.copy_verified_to(asset, &mut copied, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    assert_eq!(copied, movie);
    assert_eq!(std::fs::read(&original).unwrap(), before);
    drop(restored);
    assert_eq!(owned_entries(dir.path()), 3, "only source, workflow and imported output remain");
}

fn owned_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().filter(|e| e.as_ref().unwrap().file_name() != ".nodecraft-assets-v1.lock").count()
}

#[test]
fn cli97_comfy_workflow_size_and_command_envelope_are_refused_before_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let args = arguments(dir.path());
    for size in [MAX_WORKFLOW_BYTES, MAX_WORKFLOW_BYTES + 1] {
        let mut text = FLOW.to_owned();
        text.push_str(&" ".repeat(size - FLOW.len()));
        std::fs::write(&args.workflow, text).unwrap();
        let error = import(&args, &settings(), || panic!("oversized workflow cannot contact the server")).unwrap_err();
        assert!(error.message.contains("limit"), "{error:?}");
        assert!(!args.out.exists());
    }
    assert_eq!(owned_entries(dir.path()), 1, "failed envelope validation cleans its private staging");
}
#[test]
fn cli97_comfy_url_display_never_echoes_credentials_or_invalid_url_text() {
    let raw = "http://fixture-user:fixture-password@127.0.0.1:8188/comfy?token=fixture-query#fixture-fragment";
    assert_eq!(shown_url(raw).as_deref(), Some("http://127.0.0.1:8188/comfy"));
    for raw in ["invalid fixture-secret", "file:///fixture-secret", "data:fixture-secret"] {
        assert_eq!(shown_url(raw), None);
    }
}
