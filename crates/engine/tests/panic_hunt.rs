use nodecraft_nodes::NodeKind;
mod common;
#[path = "panic_hunt/plugins.rs"]
mod plugins;

use nodecraft_engine::Session;
use nodecraft_types::ErrorKind;
use serde_json::{Value, json};

#[test]
#[ignore = "adversarial command corpus; run explicitly when commands change"]
fn panic_hunt() {
    let _cleanup = RemoveExportFolder::hold();
    let original = common::project();
    let mut session = Session::new(original.clone()).unwrap();
    plugins::register(&mut session);
    // The generic corpus also reaches actual result-service parameter/identity checks.
    let mut result_services = nodecraft_engine::commands::Services::default();
    let worker = nodecraft_engine::plugin_cmds::PluginWorker::spawn(
        || nodecraft_plugin_store::Store::open(nodecraft_plugin_store::MemoryBackend::default()),
        nodecraft_engine::plugin_cmds::PluginHost { trust: None, taken: Default::default(), bindings: Default::default() },
    )
    .unwrap();
    // Use a separate worker-backed Session below for its two result commands; lifecycle requests
    // keep the real synchronous memory store and private file acquisition fixture.
    nodecraft_engine::plugin_cmds::register_plugin_worker(&mut result_services, worker).unwrap();
    let mut results = Session::new(original.clone()).unwrap().with_services(result_services);
    let ids: Vec<_> = session.registry().specs().map(|spec| spec.id).collect();
    let mut deep = Value::Null;
    for _ in 0..128 {
        deep = json!([deep]);
    }
    let mut corpus = vec![
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(u64::MAX),
        json!(1e100),
        json!("💥"),
        json!([]),
        json!({"unknown":true}),
        deep,
        json!("x".repeat(1024 * 1024 + 1)),
        json!(vec![Value::Null; 16_385]),
    ];
    // Exercise typed parameter decoding and graph validation, not only top-level JSON errors.
    for raw in [json!("not-an-id"), Value::Null, json!(u64::MAX), json!({}), json!([])] {
        for field in [
            "canvas",
            "id",
            "pos",
            "node",
            "edge",
            "gesture",
            "title",
            "name",
            "value",
            "replace",
            "nodes",
            "edges",
            "location",
            "discard_dirty",
            "text",
            "reply",
            "resolved",
            "expected_revision",
            "ids",
            "revision",
            "model",
            "preview_binding",
        ] {
            let mut params = json!({"canvas":common::canvas(),"id":common::node_id(1),"pos":{"x":1,"y":0}});
            params[field] = raw.clone();
            corpus.push(params);
        }
    }
    for raw in [Value::Null, json!("wrong"), json!({"type":"list","value":[null]}), json!({"type":"number","value":1e100})] {
        corpus.push(json!({"canvas":common::canvas(),"id":common::node_id(1),"name":"text","value":raw}));
        corpus.push(json!({"canvas":common::canvas(),"id":common::node_id(1),"title":raw}));
    }
    // edge.rewire and canvas.delete: duplicate, missing and oversized id lists.
    corpus.push(json!({"canvas":common::canvas(),"edge":common::edge(),"replace":[common::edge().id, common::edge().id]}));
    corpus.push(json!({"canvas":common::canvas(),"nodes":[common::node_id(1), common::node_id(1)],"edges":[common::edge().id]}));
    corpus.push(json!({"canvas":common::canvas(),"nodes":vec![common::node_id(9); 20_000]}));
    let field = nodecraft_types::FieldId::from_bytes([7; 16]);
    corpus.push(json!({"canvas":common::canvas(),"fields":[field, field]}));
    corpus.push(json!({"canvas":common::canvas(),"nodes":[common::node_id(1)],"fields":[field]}));
    corpus.push(json!({"canvas":common::canvas(),"fields":vec![field; 20_000]}));
    corpus.push(json!({"canvas":common::canvas(),"nodes":vec![common::node_id(9); 5_000],"fields":vec![field; 5_001]}));
    // Bulk decoding must be exercised with the complete envelope, beyond its target preflight.
    for raw in [Value::Null, json!("wrong"), json!({"type":"list","value":[null]}), json!({"type":"number","value":1e100})] {
        corpus.push(json!({"canvas":common::canvas(),"ids":[common::node_id(1),common::node_id(2)],"revision":0,"name":"text","value":raw}));
    }
    corpus.push(json!({"canvas":common::canvas(),"ids":vec![common::node_id(1);257],"revision":0,"name":"text","value":null}));
    corpus.push(json!({"canvas":common::canvas(),"ids":[common::node_id(1),common::node_id(1)],"revision":0,"name":"text","value":null}));
    for id in ids {
        let session = if matches!(id, "plugins.result" | "plugins.forget") { &mut results } else { &mut session };
        for params in &corpus {
            let before = session.document().clone();
            let revision = session.revision();
            let enabled = session.enabled(id, params);
            let result = session.dispatch(id, params);
            assert!(!enabled.as_ref().is_err_and(|error| error.kind == ErrorKind::Internal), "enabled panicked for {id}");
            assert!(!result.as_ref().is_err_and(|error| error.kind == ErrorKind::Internal), "dispatch panicked for {id}");
            if result.is_err() {
                assert_eq!(session.document(), &before, "{id}");
                assert_eq!(session.revision(), revision, "{id}");
            } else {
                assert!(enabled.is_ok());
                session.document().validate().unwrap();
            }
        }
    }
}

fn gen_id(n: u8) -> nodecraft_types::GenerationId {
    nodecraft_types::GenerationId::from_bytes([n; 16])
}

/// The folder `export.*` templates write into: one directory in the system temporary folder per
/// test process, so a hunt that reaches the real write path stays out of the repository. It is
/// left where a leaked `TempDir` would be, for the OS to sweep.
fn export_destination() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let path = DIR
        .get_or_init(|| {
            let path = std::env::temp_dir().join(format!("nodecraft-panic-hunt-export-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&path);
            std::fs::canonicalize(&path).unwrap_or(path)
        })
        .clone();
    // A hunt that finished may have removed the folder, so recreate it for the next user.
    let _ = std::fs::create_dir_all(&path);
    path
}

/// Hunts currently holding the shared export folder. The last one out removes it, even when an
/// assertion fails. The count and the removal share one lock, so a hunt starting as the last one
/// leaves cannot have the folder removed under it.
static HUNTS_IN_FLIGHT: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

struct RemoveExportFolder;
impl RemoveExportFolder {
    fn hold() -> Self {
        let mut count = HUNTS_IN_FLIGHT.lock().unwrap_or_else(|poison| poison.into_inner());
        *count += 1;
        Self
    }
}
impl Drop for RemoveExportFolder {
    fn drop(&mut self) {
        let mut count = HUNTS_IN_FLIGHT.lock().unwrap_or_else(|poison| poison.into_inner());
        *count -= 1;
        if *count == 0 {
            let _ = std::fs::remove_dir_all(export_destination());
        }
    }
}

fn template(id: &str) -> (Session, Value) {
    use common::*;
    let mut document = project();
    if id == "run.start" {
        document = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([0; 16]), "Run corpus").unwrap();
        nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas(), "Canvas") }.apply(&mut document).unwrap();
        let source = nodecraft_nodes::text::kind().descriptor().instantiate(node_id(1), &std::collections::BTreeMap::new()).unwrap();
        nodecraft_graph::Change::AddNode { canvas: canvas(), node: source }.apply(&mut document).unwrap();
    }
    if id == "edge.connect" {
        nodecraft_graph::Change::Disconnect { canvas: canvas(), id: edge().id }.apply(&mut document).unwrap();
    }
    if matches!(id, "node.param_set" | "node.params_set_bulk") {
        let nodes = if id == "node.params_set_bulk" { vec![3, 4] } else { vec![3] };
        for n in nodes {
            let mut source = nodecraft_nodes::text::kind().descriptor().instantiate(node_id(n), &std::collections::BTreeMap::new()).unwrap();
            source.params.insert("text".into(), nodecraft_types::Value::Text("before".into()));
            nodecraft_graph::Change::AddNode { canvas: canvas(), node: source }.apply(&mut document).unwrap();
        }
    }
    if id.starts_with("element.") {
        // An image to make the Element from, and for the sheet an Element already made of it.
        let mut seed = Session::new(document.clone()).unwrap();
        seed.dispatch("node.create", &json!({"canvas":canvas(),"id":node_id(5),"kind":"source.image","pos":{"x":0,"y":0}})).unwrap();
        if id != "element.create" {
            let from = [json!({"node":node_id(5),"port":"out"})];
            let create = json!({"canvas":canvas(),"id":node_id(6),"name":"hero","from":from,"edges":[nodecraft_types::EdgeId::from_bytes([0x76; 16])],"pos":{"x":1,"y":2}});
            seed.dispatch("element.create", &create).unwrap();
        }
        document = seed.document().clone();
    }
    if id == "document.use_pages" {
        let mut seed = Session::new(document.clone()).unwrap();
        seed.dispatch("node.create", &json!({"canvas":canvas(),"id":node_id(5),"kind":"document.pages","pos":{"x":0,"y":0}})).unwrap();
        document = seed.document().clone();
    }
    if matches!(id, "image.tools" | "image.tool") {
        let mut seed = Session::new(document.clone()).unwrap();
        seed.dispatch("node.create", &json!({"canvas":canvas(),"id":node_id(5),"kind":"source.image","pos":{"x":0,"y":0}})).unwrap();
        document = seed.document().clone();
    }
    if id.starts_with("shader.") {
        let mut seed = Session::new(document.clone()).unwrap();
        seed.dispatch("node.create", &json!({"canvas":canvas(),"id":node_id(0x5a),"kind":"image.shader","pos":{"x":0,"y":0}})).unwrap();
        document = seed.document().clone();
    }
    if matches!(id, "mesh.capture" | "mesh.view_set") {
        let mut seed = Session::new(document.clone()).unwrap();
        seed.dispatch("node.create", &json!({"canvas":canvas(),"id":node_id(5),"kind":"source.mesh","pos":{"x":0,"y":0}})).unwrap();
        document = seed.document().clone();
    }
    if matches!(id, "node.model_preview" | "node.set_model" | "node.prompt_assist") {
        let seed = Session::new(document.clone()).unwrap();
        let supplied = [
            ("model".into(), nodecraft_types::Value::Text("openai-images.sunburst".into())),
            ("prompt".into(), nodecraft_types::Value::Text("Public-safe corpus prompt".into())),
        ]
        .into();
        let node = seed.kinds().get("generation.image").unwrap().instantiate_instance(node_id(7), Default::default(), &supplied).unwrap();
        nodecraft_graph::Change::AddNode { canvas: canvas(), node }.apply(&mut document).unwrap();
    }
    if id == "comfy.server.use" {
        let seed = Session::new(document.clone()).unwrap();
        let supplied = [("model".into(), nodecraft_types::Value::Text("comfyui.sdxl".into()))].into();
        let node = seed.kinds().get("generation.image").unwrap().instantiate_instance(node_id(7), Default::default(), &supplied).unwrap();
        nodecraft_graph::Change::AddNode { canvas: canvas(), node }.apply(&mut document).unwrap();
    }
    if id.starts_with("field.") && id != "field.create" || id == "node.field_set" {
        let field = nodecraft_graph::Field::new(nodecraft_types::FieldId::from_bytes([8; 16]), "Before");
        nodecraft_graph::Change::PutField { canvas: canvas(), field }.apply(&mut document).unwrap();
    }
    if matches!(id, "tag.put" | "tag.remove" | "node.tag_set") {
        let tag = nodecraft_types::TagId::from_bytes([9; 16]);
        nodecraft_graph::Changeset::new(vec![
            nodecraft_graph::Change::PutTag { canvas: canvas(), tag: nodecraft_graph::Tag { id: tag, title: "Before".into(), color: None } },
            nodecraft_graph::Change::SetNodeTags {
                canvas: canvas(),
                assignments: vec![nodecraft_graph::NodeTagAssignment { node: node_id(1), tag: Some(tag) }],
            },
        ])
        .apply(&mut document)
        .unwrap();
    }
    if id.starts_with("layers.") || id == "node.layers_set" {
        // A Layer Editor (node 9) holding a text layer `t1` (not yet for `layers.add`), so the
        // valid templates reach the layer code rather than the kind check.
        let mut editor = nodecraft_nodes::layers::kind().descriptor().instantiate(node_id(9), &std::collections::BTreeMap::new()).unwrap();
        if id != "layers.add" {
            let stored = json!({"layers": [{"id":"t1","name":"Title","x":50,"y":20,"width":80,"height":30,"content":{"kind":"text","text":"Hi","size":24,"color":"#000000"}}]});
            editor.config.insert(nodecraft_nodes::layers::CONFIG.into(), nodecraft_types::Value::Text(stored.to_string()));
        }
        nodecraft_graph::Change::AddNode { canvas: canvas(), node: editor }.apply(&mut document).unwrap();
    }
    if id.starts_with("deck.") && id != "deck.create" {
        // A Deck (node 9) holding a slide `s1` with a text layer `t1` and a source layer on node 1,
        // so the valid templates reach the slide code rather than the kind check.
        let mut deck = nodecraft_nodes::deck::kind().descriptor().instantiate(node_id(9), &std::collections::BTreeMap::new()).unwrap();
        let stored = json!({"version": 1, "slides": [{"id": "s1", "name": "One", "layers": [
            {"id":"t1","name":"Title","x":50,"y":20,"width":80,"height":30,"content":{"kind":"text","text":"Hi","size":24,"color":"#000000"}},
            {"id":"p1","name":"Picture","x":0,"y":100,"width":200,"height":100,"content":{"kind":"source","origin":node_id(1).to_string(),"nth":0}}
        ]}]});
        deck.config.insert(nodecraft_nodes::deck::CONFIG.into(), nodecraft_types::Value::Text(stored.to_string()));
        nodecraft_graph::Change::AddNode { canvas: canvas(), node: deck }.apply(&mut document).unwrap();
    }
    if id.starts_with("timeline.") || id == "node.timeline_set" {
        // A Timeline (node 9) holding a clip `t1` with a known end, an empty audio track 1 and a caption `c1`, so the valid templates reach
        // the clip code rather than the kind check.
        let mut editor = nodecraft_nodes::timeline::kind().descriptor().instantiate(node_id(9), &std::collections::BTreeMap::new()).unwrap();
        let stored = json!({"clips": [{"id":"t1","origin":"n1","in_us":0,"out_us":2_000_000}], "tracks": [{"kind":"audio"}], "captions": [{"id":"c1","text":"Hi","start_us":0,"end_us":1_000_000}]});
        editor.config.insert(nodecraft_nodes::timeline::CONFIG.into(), nodecraft_types::Value::Text(stored.to_string()));
        nodecraft_graph::Change::AddNode { canvas: canvas(), node: editor }.apply(&mut document).unwrap();
    }
    let root = nodecraft_types::CommentId::from_bytes([10; 16]);
    let reply = nodecraft_types::CommentId::from_bytes([11; 16]);
    if id.starts_with("comment.") {
        let mut comment = nodecraft_graph::Comment::new(root, "Original", nodecraft_graph::Position::default());
        comment.replies.push(nodecraft_graph::CommentReply::new(reply, "Reply"));
        nodecraft_graph::Change::PutComment { canvas: canvas(), comment }.apply(&mut document).unwrap();
    }
    if id.starts_with("history.") {
        // Two results on node 1, oldest first; `history.next` starts at the older one.
        for n in [0x21, 0x22] {
            let generation = nodecraft_graph::Generation::new(gen_id(n), vec![nodecraft_types::Value::Text(format!("take {n}"))]);
            nodecraft_graph::Change::AddGeneration { canvas: canvas(), id: node_id(1), index: usize::from(n - 0x21), generation }.apply(&mut document).unwrap();
        }
        let active = if id == "history.next" { 0x21 } else { 0x22 };
        nodecraft_graph::Change::SetActiveGeneration { canvas: canvas(), id: node_id(1), generation: Some(gen_id(active)) }.apply(&mut document).unwrap();
    }
    let mut session = if id.starts_with("file.") {
        let mut storage = nodecraft_format::MemoryStorage::default();
        let snapshot = nodecraft_format::Snapshot { project: document.clone(), assets: Default::default(), epoch: 0 };
        nodecraft_format::ProjectStorage::save(
            &mut storage,
            &nodecraft_format::StorageLocation::from_command("template.ncraft".into()).unwrap(),
            &nodecraft_format::encode(&snapshot).unwrap(),
        )
        .unwrap();
        let mut session = Session::with_storage(snapshot, Box::new(storage)).unwrap();
        session.dispatch("file.save_as", &json!({"location":"template.ncraft"})).unwrap();
        session
    } else {
        Session::new(document).unwrap()
    };
    if id == "file.focus_owner" {
        session.register_host_service(nodecraft_engine::storage_window_cmds::SERVICE, |_| Ok(json!({"requested":true}))).unwrap();
    }
    if matches!(id, "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" | "canvas.prompt_overlays_toggle" | "canvas.pan_to") {
        session.swap_resource(Some(nodecraft_engine::navigation_cmds::NavigationContext {
            canvas: canvas(),
            viewport_available: true,
            blocked: false,
            fit_all: true,
            fit_selection: true,
        }));
        session.register_host_service(nodecraft_engine::navigation_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    if matches!(id, "document.extract_page" | "document.extract_text") {
        let bytes = nodecraft_media::pdf_fixture::pdf(&["BT /F1 12 Tf 20 150 Td (Public extraction corpus) Tj ET"]);
        nodecraft_engine::import::import_files(
            &mut session,
            canvas(),
            nodecraft_graph::Position::default(),
            0.0,
            &[nodecraft_engine::import::Incoming { name: "public.pdf", bytes: &bytes }],
            &[node_id(5)],
        )
        .unwrap();
    }
    if id.starts_with("canvas.search_") {
        session.swap_resource(Some(nodecraft_engine::search_cmds::SearchContext {
            canvas: canvas(),
            viewport_available: true,
            blocked: false,
            open: true,
            has_query: true,
        }));
        session.register_host_service(nodecraft_engine::search_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    if id == "canvas.auto_format" {
        session.swap_resource(Some(common::layout_context(&session)));
    }
    if matches!(id, "comment.open" | "comment.compose" | "canvas.comments_toggle") {
        session.swap_resource(Some(nodecraft_engine::comment_cmds::CommentContext {
            project: session.document().id(),
            canvas: canvas(),
            revision: session.revision(),
            blocked: false,
        }));
        session.register_host_service(nodecraft_engine::comment_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    if matches!(id, "template.refresh" | "template.retrieve") {
        session
            .register_host_service(nodecraft_engine::templates::fetch::SERVICE, |request| {
                let _: nodecraft_engine::templates::fetch::Request = serde_json::from_value(request.clone()).unwrap();
                Ok(json!({"pending":1}))
            })
            .unwrap();
    }
    if id.starts_with("export.local_") || id.starts_with("export.results_") {
        // A real destination, so the hunt reaches name rendering and the write path rather than
        // stopping at the host check. It is in the system temporary folder, never the repository.
        session.swap_resource(Some(nodecraft_engine::local_export::ExportHost::authorize(&export_destination()).unwrap()));
    }
    if id.starts_with("run.") || id == "usage.export_csv" {
        use nodecraft_engine::run_cmds::*;
        session.swap_resource(Some(RunCatalog::new(nodecraft_catalog::Catalog::builtin().unwrap()).unwrap()));
        session.swap_resource(Some(BudgetState { budgets: Default::default(), spent: Default::default() }));
        session.swap_resource(Some(UsageSnapshot(std::sync::Arc::new(Default::default()))));
        session.register_run_host(|_| Ok(json!({"run":"accepted"}))).unwrap();
    }
    if id == "history.copy" {
        session.register_clipboard_host(|_| Ok(())).unwrap();
    }
    if id == "usage.unsettled" || id == "usage.resolve_charge" {
        use nodecraft_engine::resolve_cmds::*;
        // One reconciled open charge, so adversarial params reach the token and amount checks.
        let log = reconciled_legacy();
        session.swap_resource(Some(nodecraft_engine::run_cmds::UsageSnapshot(std::sync::Arc::new(log.clone()))));
        let log = std::sync::Mutex::new(log);
        session.register_host_service(RESOLUTION_SERVICE, move |request| resolve(&mut log.lock().unwrap(), request, 0)).unwrap();
    }
    if id.starts_with("usage.ownership") || id == "usage.reconcile_ownership" {
        use nodecraft_engine::ownership_cmds::*;
        // One quarantined group, so adversarial params reach token and holder checks.
        let legacy: nodecraft_exec::cost::UsageLog =
            serde_json::from_str(include_str!("../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
        session.swap_resource(Some(nodecraft_engine::run_cmds::UsageSnapshot(std::sync::Arc::new(legacy.clone()))));
        let log = std::sync::Arc::new(std::sync::Mutex::new(legacy));
        session.register_host_service(OWNERSHIP_SERVICE, ownership_service(log, std::sync::Arc::new(|| 0))).unwrap();
    }
    if id.starts_with("canvas.tag_filter_") {
        session.swap_resource(Some(nodecraft_engine::tag_filter_cmds::TagFilterContext {
            canvas: canvas(),
            viewport_available: true,
            blocked: false,
            open: true,
            active: true,
            scanning: false,
            matches: 1,
        }));
        session.register_host_service(nodecraft_engine::tag_filter_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
    }
    // A Batch Generate sheet with two rows and a text source to wire into them.
    let sheet = nodecraft_types::CanvasId::from_bytes([0x51; 16]);
    let sheet_rows: Vec<Value> = if id.starts_with("sheet.") && id != "sheet.create" {
        session.dispatch("sheet.create", &json!({"canvas":sheet,"title":"Sheet"})).unwrap();
        let made = session.dispatch("sheet.rows_add", &json!({"canvas":sheet,"seed":node_id(0x52),"prompts":["one","two"]})).unwrap();
        let note = nodecraft_nodes::text::kind().descriptor().instantiate(node_id(0x53), &std::collections::BTreeMap::new()).unwrap();
        session.dispatch("node.add", &json!({"canvas":sheet,"node":note})).unwrap();
        if id == "sheet.reference_unwire" {
            session.dispatch("sheet.reference_wire", &json!({"canvas":sheet,"seed":node_id(0x55),"reference":node_id(0x53)})).unwrap();
        }
        made["rows"].as_array().unwrap().clone()
    } else {
        Vec::new()
    };
    // A text source on the canvas, and a published technique version of it.
    let technique_canvas = nodecraft_types::CanvasId::from_bytes([0x61; 16]);
    let technique = nodecraft_types::TechniqueId::from_bytes([0x62; 16]);
    if id.starts_with("library.") {
        nodecraft_engine::library::install(&mut session, Box::new(nodecraft_engine::library::MemoryLibrary::default())).unwrap();
    }
    if id.starts_with("element.") || id.ends_with("_element") || id == "library.elements" {
        // An Elements library holding "hero": one image, kept by its hash.
        use nodecraft_engine::element_library::{ElementStore, MemoryElements};
        let store = MemoryElements::default();
        let pixels = [10, 80, 160, 255].repeat(4);
        let png = nodecraft_media::encode_png(2, 2, &pixels, &nodecraft_media::Limits::default()).unwrap();
        let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(&png).as_bytes());
        store.put_media(&hash, &png).unwrap();
        let image = json!({"hash": hash, "mime": "image/png"});
        store.save(&serde_json::to_vec(&json!({"version": 1, "elements": [{"name": "hero", "category": "character", "images": [image]}]})).unwrap()).unwrap();
        nodecraft_engine::element_library::install(&mut session, std::rc::Rc::new(store));
    }
    if matches!(id, "shader.save" | "shader.place" | "shader.update" | "library.shaders" | "library.remove_shader") {
        // A shader library holding "Glow" version 1.
        use nodecraft_engine::library::LibraryStore;
        let store = nodecraft_engine::library::MemoryLibrary::default();
        let glow = json!({"source": "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }", "params": []});
        store
            .save(&serde_json::to_vec(&json!({"version": 1, "shaders": [{"name": "Glow", "next": 2, "versions": [{"version": 1, "shader": glow}]}]})).unwrap())
            .unwrap();
        nodecraft_engine::shader_library::install(&mut session, Box::new(store)).unwrap();
    }
    let skill = nodecraft_types::SkillId::from_bytes([0x69; 16]);
    if id.starts_with("skill.") {
        nodecraft_engine::skills::install(&mut session, std::rc::Rc::new(nodecraft_engine::skills::MemorySkills::default())).unwrap();
        if !matches!(id, "skill.create" | "skill.import" | "skill.list") {
            session.dispatch("skill.create", &json!({"scope":"project","id":skill,"name":"Storyboard","instructions":"Six panels."})).unwrap();
        }
    }
    let app_form = matches!(id, "app.form" | "app.input_set");
    if id.starts_with("technique.") || id.starts_with("library.") || app_form {
        let note = nodecraft_nodes::text::kind().descriptor().instantiate(node_id(0x63), &std::collections::BTreeMap::new()).unwrap();
        session.dispatch("node.add", &json!({"canvas":canvas(),"node":note})).unwrap();
        if !matches!(id, "technique.check" | "technique.publish" | "technique.list") {
            let mut publish = json!({"canvas":canvas(),"published":technique_canvas,"seed":node_id(0x64),"technique":technique,"name":"Note","outputs":[{"node":node_id(0x63)}]});
            if app_form {
                // A technique with an input: the note feeding a replace.
                let kinds = nodecraft_nodes::Registry::builtins().unwrap();
                let replace = kinds.get("text.replace").unwrap().instantiate_instance(node_id(0x67), Default::default(), &Default::default()).unwrap();
                session.dispatch("node.add", &json!({"canvas":canvas(),"node":replace})).unwrap();
                let wire = json!({"id":nodecraft_types::EdgeId::from_bytes([0x68; 16]),"from":{"node":node_id(0x63),"port":"out"},"to":{"node":node_id(0x67),"port":"text"},"order":0});
                session.dispatch("edge.connect", &json!({"canvas":canvas(),"edge":wire})).unwrap();
                publish["inputs"] = json!([{"node":node_id(0x63),"name":"Brief"}]);
                publish["outputs"] = json!([{"node":node_id(0x67)}]);
            }
            session.dispatch("technique.publish", &publish).unwrap();
        }
        if matches!(id, "library.remove" | "library.place") {
            session.dispatch("library.add", &json!({"canvas":technique_canvas})).unwrap();
            session.dispatch("library.list", &json!({})).unwrap();
        }
        if matches!(id, "technique.upgrade" | "technique.detach") || app_form {
            let place = json!({"canvas":canvas(),"version":technique_canvas,"id":node_id(0x65),"pos":{"x":0,"y":0}});
            session.dispatch("technique.place", &place).unwrap();
        }
    }
    if matches!(id, "comfy.workflow.import" | "comfy.workflow.mapping" | "comfy.workflow.mapping_set") {
        // The server's nodes as comfy.nodes.refresh installs them, so the import succeeds.
        let info = br#"{"CLIPTextEncode": {"input": {"required": {"text": ["STRING", {}]}}, "output": ["IMAGE"]}, "SaveImage": {"input": {"required": {"images": ["IMAGE", {}]}}, "output": [], "output_node": true}}"#;
        let catalog = nodecraft_comfy::NodeCatalog::parse(info).unwrap().0;
        nodecraft_engine::comfy_workflow_cmds::finish(&mut session, 1, Ok(std::sync::Arc::new(catalog).into()));
    }
    if matches!(id, "comfy.workflow.mapping" | "comfy.workflow.mapping_set") {
        session
            .dispatch(
                "comfy.workflow.import",
                &json!({"canvas":canvas(),"id":node_id(0x69),"pos":{"x":0,"y":0},"workflow":
            r#"{"1":{"class_type":"CLIPTextEncode","inputs":{"text":"a fox"}},"2":{"class_type":"SaveImage","inputs":{"images":["1",0]}}}"#}),
            )
            .unwrap();
    }
    let plugin_old =
        if id.starts_with("plugins.") && !matches!(id, "plugins.result" | "plugins.forget") { Some(plugins::register(&mut session)) } else { None };
    let pending = if matches!(id, "plugins.result" | "plugins.forget") { Some(plugins::register_results(&mut session)) } else { None };
    let movement = json!({"canvas":canvas(),"id":node_id(1),"pos":{"x":12,"y":0}});
    let params = match id {
        "tag.put" => json!({"canvas":canvas(),"tag":{"id":nodecraft_types::TagId::from_bytes([9;16]),"title":"After","color":"blue"}}),
        "tag.remove" => json!({"canvas":canvas(),"tag":nodecraft_types::TagId::from_bytes([9;16])}),
        "node.tag_set" => json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":null}),
        "field.create" => {
            json!({"canvas":canvas(),"field":{"id":nodecraft_types::FieldId::from_bytes([8;16]),"title":"New","rect":{"pos":{"x":0,"y":0},"width":320,"height":240}},"nodes":[node_id(1)]})
        }
        "field.update" => json!({"canvas":canvas(),"field":nodecraft_types::FieldId::from_bytes([8;16]),"collapsed":true}),
        "field.move" => json!({"canvas":canvas(),"field":nodecraft_types::FieldId::from_bytes([8;16]),"delta":{"x":12,"y":3}}),
        "field.remove" => json!({"canvas":canvas(),"field":nodecraft_types::FieldId::from_bytes([8;16])}),
        "node.field_set" => json!({"canvas":canvas(),"nodes":[node_id(1)],"field":nodecraft_types::FieldId::from_bytes([8;16])}),
        "node.migrate" => json!({"canvas":canvas(),"node":node_id(1)}),
        "canvas.tag_filter_open" | "canvas.tag_filter_close" | "canvas.tag_filter_next" | "canvas.tag_filter_previous" => json!({}),
        "canvas.tag_filter_set" => json!({"tags":[],"untagged":true}),
        "comment.create" => json!({"canvas":canvas(),"id":nodecraft_types::CommentId::from_bytes([20;16]),"text":"New","pos":{"x":1,"y":2}}),
        "comment.edit" => json!({"canvas":canvas(),"id":root,"text":"Edited"}),
        "comment.reply" => json!({"canvas":canvas(),"id":root,"reply":nodecraft_types::CommentId::from_bytes([12;16]),"text":"New reply"}),
        "comment.reply_remove" => json!({"canvas":canvas(),"id":root,"reply":reply}),
        "comment.move" => json!({"canvas":canvas(),"id":root,"pos":{"x":12,"y":24},"node":node_id(1)}),
        "comment.resolve" => json!({"canvas":canvas(),"id":root,"resolved":true}),
        "comment.remove" => json!({"canvas":canvas(),"id":root}),
        "comment.open" => json!({"id":root}),
        "comment.compose" => json!({"pos":{"x":12,"y":24}}),
        "canvas.comments_toggle" => json!({}),
        "node.add" => json!({"canvas":canvas(),"node":node(3)}),
        "node.remove" => json!({"canvas":canvas(),"id":node_id(1)}),
        "node.move" => movement.clone(),
        "node.title" => json!({"canvas":canvas(),"id":node_id(1),"title":"Renamed"}),
        "node.param_set" => json!({"canvas":canvas(),"id":node_id(3),"name":"text","value":null}),
        "node.params_set_bulk" => json!({"canvas":canvas(),"ids":[node_id(3),node_id(4)],"revision":0,"name":"text","value":null}),
        "node.model_preview" => json!({"canvas":canvas(),"nodes":[node_id(7)],"model":"openai-images.flare"}),
        "node.prompt_assist" => json!({"canvas":canvas(),"node":node_id(7),"id":node_id(0x58),"edge":nodecraft_types::EdgeId::from_bytes([0x58; 16])}),
        "node.set_model" => {
            let mut request = json!({"canvas":canvas(),"nodes":[node_id(7)],"model":"openai-images.flare"});
            let preview = session.dispatch("node.model_preview", &request).unwrap();
            request["expected_revision"] = preview["revision"].clone();
            request["preview_binding"] = preview["binding"].clone();
            request
        }
        "edge.connect" => json!({"canvas":canvas(),"edge":edge()}),
        "edge.disconnect" => json!({"canvas":canvas(),"id":edge().id}),
        "canvas.auto_format" => json!({"mode":"grid"}),
        // The fixture's inputs take one noodle each, so this is refused; the hunt checks it never panics.
        "edge.reorder" => json!({"canvas":canvas(),"to":{"node":node_id(2),"port":"in"},"edges":[edge().id]}),
        "canvas.stale" => json!({"canvas":canvas()}),
        "sheet.create" => json!({"canvas":nodecraft_types::CanvasId::from_bytes([0x50; 16]),"title":"Sheet"}),
        "sheet.list" => json!({}),
        "sheet.rows" => json!({"canvas":sheet}),
        "sheet.rows_add" => json!({"canvas":sheet,"seed":node_id(0x54),"prompts":["three"],"after":sheet_rows[0]}),
        "sheet.rows_duplicate" => json!({"canvas":sheet,"seed":node_id(0x54),"rows":[sheet_rows[0]]}),
        "sheet.rows_remove" => json!({"canvas":sheet,"rows":[sheet_rows[0], sheet_rows[1]]}),
        "sheet.row_move" => json!({"canvas":sheet,"row":sheet_rows[1],"index":0}),
        "sheet.reference_wire" => json!({"canvas":sheet,"seed":node_id(0x54),"reference":node_id(0x53),"rows":[sheet_rows[1]]}),
        "sheet.reference_unwire" => json!({"canvas":sheet,"reference":node_id(0x53),"rows":[sheet_rows[0]]}),
        "sheet.expand" => {
            json!({"canvas":sheet,"seed":node_id(0x56),"template":"a {{colour}} fox","variables":[{"name":"colour","text":["red","blue"]}],"copies":2})
        }
        "technique.check" => json!({"canvas":canvas(),"outputs":[{"node":node_id(0x63)}]}),
        "technique.publish" => {
            json!({"canvas":canvas(),"published":technique_canvas,"seed":node_id(0x64),"technique":technique,"name":"Note","outputs":[{"node":node_id(0x63)}]})
        }
        "technique.list" => json!({}),
        "technique.show" | "technique.remove" => json!({"canvas":technique_canvas}),
        "technique.place" => json!({"canvas":canvas(),"version":technique_canvas,"id":node_id(0x65),"pos":{"x":0,"y":0}}),
        "skill.list" => json!({"query":"story"}),
        "skill.read" | "skill.export" => json!({"id":skill}),
        "skill.create" => json!({"scope":"user","id":skill,"name":"Storyboard","instructions":"Six panels.","requires":{"commands":["node.add"]}}),
        "skill.edit" => json!({"id":skill,"version":1,"instructions":"Eight panels."}),
        "skill.enable" => json!({"id":skill,"version":1,"enabled":false}),
        "skill.delete" => json!({"id":skill,"version":1}),
        "skill.import" => {
            let text = json!({"format":"nodecraft.skill","version":1,"skill":{"name":"Storyboard","instructions":"Six panels."}}).to_string();
            json!({"scope":"user","id":skill,"text":text,"origin":"a file"})
        }
        "library.list" => json!({}),
        "template.list" => json!({}),
        "template.add" => json!({"id":"generate-text"}),
        "template.refresh" => json!({}),
        "template.retrieve" => {
            let entry = nodecraft_engine::templates::entries().unwrap().into_iter().find(|entry| entry.id == "generate-text").unwrap();
            json!({"id":entry.id,"version":entry.version,"blake3":entry.blake3})
        }
        "library.add" => json!({"canvas":technique_canvas}),
        "library.remove" => json!({"technique":technique,"version":1}),
        "library.place" => {
            json!({"canvas":canvas(),"technique":technique,"version":1,"id":node_id(0x65),"pos":{"x":0,"y":0},"copy":nodecraft_types::CanvasId::from_bytes([0x66; 16])})
        }
        "technique.upgrade" => json!({"canvas":canvas(),"node":node_id(0x65),"version":technique_canvas}),
        "technique.detach" => json!({"canvas":canvas(),"node":node_id(0x65),"seed":node_id(0x66)}),
        "app.form" => json!({"canvas":canvas(),"id":node_id(0x65)}),
        "app.input_set" => json!({"canvas":canvas(),"id":node_id(0x65),"port":"brief","seed":node_id(0x66),"values":[{"type":"text","value":"a fox"}]}),
        "node.describe" => json!({"canvas":canvas(),"id":node_id(1)}),
        "node.options" => json!({"canvas":canvas(),"id":node_id(1),"param":"text"}),
        "options.refresh" | "options.status" => json!({}),
        // No fetch service here: the refresh must be refused cleanly.
        "comfy.nodes.refresh" | "comfy.nodes.status" => json!({}),
        "comfy.server.use" => json!({"canvas":canvas(),"id":node_id(7),"server":"studio"}),
        "comfy.workflow.import" => json!({"canvas":canvas(),"id":node_id(0x69),"pos":{"x":0,"y":0},"workflow":
            r#"{"1": {"class_type": "CLIPTextEncode", "inputs": {"text": "a fox"}}, "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0]}}}"#}),
        "comfy.workflow.mapping" => json!({"canvas":canvas(),"id":node_id(0x69)}),
        "comfy.workflow.mapping_set" => {
            let fields = session.dispatch("comfy.workflow.mapping", &json!({"canvas":canvas(),"id":node_id(0x69)})).unwrap();
            json!({"canvas":canvas(),"id":node_id(0x69),"binding":fields["binding"],
                "inputs":[{"node":"1","input":"text","name":"Prompt renamed"}],
                "outputs":[{"node":"2","label":"Image"}],
                "output_names":[{"modality":"image","name":"images"}],"disconnect":false,"clear_history":false})
        }
        "providers.settings_get" => json!({}),
        "providers.settings_set" => json!({"comfyui_url":"http://127.0.0.1:8188","litellm_url":"https://proxy.example"}),
        // The updates service isn't registered here: the check must be refused cleanly.
        "app.check_updates" | "app.update_status" | "app.install_update" | "app.restart_to_update" => json!({}),
        "edge.rewire" => {
            let moved = nodecraft_graph::Edge { id: nodecraft_types::EdgeId::from_bytes([9; 16]), ..edge() };
            json!({"canvas":canvas(),"edge":moved,"replace":[edge().id]})
        }
        "canvas.paste" => json!({"canvas":canvas(),"nodes":[node(3)],"edges":[]}),
        "run.quote" => json!({"canvas":canvas(),"mode":"selected","nodes":[node_id(1)]}),
        "run.estimate" => json!({"canvas":canvas(),"mode":"selected","nodes":[node_id(1)]}),
        "run.start" => {
            let mut request = json!({"canvas":canvas(),"mode":"selected","nodes":[node_id(1)]});
            let quote = session.dispatch("run.quote", &request).unwrap();
            request["accept"] = json!({"token":quote["token"],"amounts":{},"partial":true});
            request
        }
        "run.record" => {
            json!({"canvas":canvas(),"results":[{"node":node_id(1),"generation":{"id":"0b0b0b0b-0b0b-4b0b-8b0b-0b0b0b0b0b0b","outputs":[{"type":"text","value":"x"}]}}]})
        }
        "usage.export_csv" => json!({}),
        // A real folder plus the fixture's own node, so a hunt reaches name rendering, collision
        // and per-entry failure rather than stopping at the host check.
        "export.local_plan" | "export.local_run" => json!({
            "canvas":canvas(),
            "sources":[{"node":node_id(1),"port":"out"},{"node":node_id(2),"port":"out","item":0}],
            "destination":export_destination().display().to_string(),
            "template":"{title}-{index}",
            "on_collision":"numbered",
            "format":{"mode":"original"}
        }),
        "export.results_plan" | "export.results_start" => json!({
            "canvas":canvas(),"nodes":[node_id(1)],"history":"all",
            "destination":export_destination().display().to_string(),"template":"{title}-{generation}-{index}"
        }),
        "perf.start" | "perf.clear" | "perf.report" => json!({}),
        // Stop needs a running capture; the hunt checks the refusal never panics.
        "perf.stop" => json!({}),
        "usage.ownership" => json!({"session":"a","currency":"USD","offset":0,"limit":1}),
        "usage.reconcile_ownership" => {
            // A real token for the fixture's group, so mutations reach the holder and exception
            // checks. Not a dry run, so the host service's path is hunted too; mutating
            // `dry_run` to true hunts the preview.
            let legacy: nodecraft_exec::cost::UsageLog =
                serde_json::from_str(include_str!("../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
            let token: String = legacy.ownership_group("a", nodecraft_types::Currency::USD).unwrap().token.iter().map(|b| format!("{b:02x}")).collect();
            json!({"session":"a","currency":"USD","token":token,"holder":"earlier","except":[{"provider":"t","key":"old"}],"only":[{"provider":"t","key":"old"}],"dry_run":false})
        }
        "usage.unsettled" => json!({"offset":0,"limit":1}),
        "usage.resolve_charge" => {
            // A real token for the fixture's reconciled charge; not a dry run, so the service's
            // path is hunted too.
            let token: String = reconciled_legacy().resolvable_charges()[0].token.iter().map(|b| format!("{b:02x}")).collect();
            json!({"provider":"t","key":"old","token":token,"outcome":"settled","currency":"USD","micros":1,"dry_run":false})
        }
        "history.select" | "history.delete" => json!({"canvas":canvas(),"id":node_id(1),"generation":gen_id(0x21)}),
        "history.previous" | "history.next" | "history.clear" => json!({"canvas":canvas(),"id":node_id(1)}),
        "history.extract" => {
            json!({"canvas":canvas(),"id":node_id(1),"generation":gen_id(0x21),"node":node_id(7),"pos":{"x":1,"y":2}})
        }
        "history.copy" => json!({"canvas":canvas(),"id":node_id(1),"generation":gen_id(0x21)}),
        // A text result never writes, so the hunt leaves no files behind.
        "history.export" => json!({"canvas":canvas(),"id":node_id(1),"generation":gen_id(0x21),"path":"/nodecraft-panic-hunt/out.png"}),
        "export.status" => json!({}),
        "export.cancel" => json!({"id":1}),
        "node.create" => json!({"canvas":canvas(),"id":node_id(7),"kind":"source.text","pos":{"x":1,"y":2}}),
        "node.create_connected" => json!({"canvas":canvas(),"id":node_id(7),"kind":"structure.router","pos":{"x":1,"y":2},
            "from":{"node":node_id(1),"port":"out"},"edge":nodecraft_types::EdgeId::from_bytes([0x77; 16])}),
        "canvas.delete" => json!({"canvas":canvas(),"nodes":[node_id(1)],"edges":[edge().id]}),
        "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" | "canvas.prompt_overlays_toggle" => json!({}),
        "canvas.search_open" | "canvas.search_next" | "canvas.search_previous" | "canvas.search_close" => json!({}),
        "canvas.search_query" => json!({"query":"needle"}),
        "canvas.pan_to" => json!({"x":0,"y":0}),
        "edit.undo" => {
            session.dispatch("node.move", &movement).unwrap();
            Value::Null
        }
        "edit.redo" => {
            session.dispatch("node.move", &movement).unwrap();
            session.dispatch("edit.undo", &Value::Null).unwrap();
            json!({})
        }
        // The keys service isn't registered here: dispatch must fail cleanly, never panic.
        "keys.set" => json!({"account":"openai","key":"sk-test-123456789"}),
        "keys.remove" | "keys.status" | "keys.test" => json!({"account":"openai"}),
        "file.open" => json!({"location":"template.ncraft","discard_dirty":true}),
        "file.save" => Value::Null,
        "file.save_as" => json!({"location":"new.ncraft"}),
        "file.cancel" | "file.focus_owner" => json!({}),
        // Real memory store and private package files, never caller-selected filesystem paths.
        "plugins.list" | "plugins.prune" => json!({}),
        "plugins.import" | "plugins.update" => json!({"location":"palette.ncraft-plugin","dry_run":true}),
        "plugins.enable" | "plugins.disable" | "plugins.reload" | "plugins.uninstall" | "plugins.grant" | "plugins.rights" => {
            json!({"package_id":"io.example.palette"})
        }
        "plugins.revoke" => json!({"grant_id":"g1"}),
        "plugins.rollback" => json!({"package_id":"io.example.palette","digest":plugin_old.unwrap()}),
        "plugins.result" | "plugins.forget" => json!({"pending":pending.unwrap()}),
        "run.reuse" => json!({"canvas":canvas()}),
        "run.held" => json!({}),
        "run.held_retry" | "run.held_place" | "run.held_discard" => json!({"id":1}),
        "share.links" => json!({}),
        "share.link_create" => json!({"role":"comment","name":"Ana"}),
        "share.link_update" => json!({"id":"0".repeat(64),"role":"view","name":"Ana"}),
        "share.link_revoke" => json!({"id":"0".repeat(64)}),
        "canvas.set_private" => json!({"canvas":canvas(),"private":true}),
        "share.start" => json!({"relay":"ws://127.0.0.1:9"}),
        "share.join" => json!({"link":format!("ws://127.0.0.1:9/#room={}", "0".repeat(64))}),
        "share.stop" | "share.status" => json!({}),
        "node.shader_set" => json!({"canvas":canvas(),"id":node_id(1),"source":"fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }"}),
        "shader.write" => json!({"canvas":canvas(),"node":node_id(0x5a),"id":node_id(0x5b),"request":"Blur the image a little"}),
        "shader.apply" => json!({"canvas":canvas(),"id":node_id(0x5a),"from":node_id(1)}),
        "node.cache_set" => json!({"canvas":canvas(),"id":node_id(1),"enabled":true}),
        "node.variations_set" => json!({"canvas":canvas(),"id":node_id(1),"count":3}),
        "canvas.set_variation_cards" => json!({"canvas":canvas(),"cards":true}),
        "node.mask_set" => json!({"canvas":canvas(),"id":node_id(1),"strokes":[{"size":0.1,"hardness":0.5,"points":[[0.5,0.5]]}]}),
        "mask.create" => json!({"canvas":canvas(),"from":{"node":node_id(1),"port":"out"},"expand":node_id(7),"mask":node_id(8),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16]),nodecraft_types::EdgeId::from_bytes([0x78; 16])],"pos":{"x":1,"y":2}}),
        "mask.margins_set" => json!({"canvas":canvas(),"id":node_id(1),"margins":{"left":0,"top":0,"right":64,"bottom":0}}),
        "mask.edit" => json!({"canvas":canvas(),"mask":node_id(1),"id":node_id(7),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16]),nodecraft_types::EdgeId::from_bytes([0x78; 16])],"pos":{"x":1,"y":2},"prompt":"a door"}),
        "image.tools" => json!({"canvas":canvas(),"from":{"node":node_id(5),"port":"out"}}),
        "image.tool" => json!({"canvas":canvas(),"tool":"grade","from":{"node":node_id(5),"port":"out"},"id":node_id(7),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16])],"pos":{"x":1,"y":2}}),
        "mask.refit" => json!({"canvas":canvas(),"id":node_id(1),"fit":true}),
        "mask.status" => json!({"canvas":canvas()}),
        "layers.list" => json!({"canvas":canvas(),"id":node_id(9)}),
        "node.layers_set" => {
            json!({"canvas":canvas(),"id":node_id(9),"layers":[{"id":"t1","name":"Title","x":60,"y":20,"width":80,"height":30,"content":{"kind":"text","text":"Hi","size":24,"color":"#000000"}}]})
        }
        "layers.add" => {
            json!({"canvas":canvas(),"id":node_id(9),"layer":{"id":"t1","name":"Title","x":50,"y":20,"width":80,"height":30,"content":{"kind":"text","text":"Hi","size":24,"color":"#000000"}}})
        }
        "layers.update" => json!({"canvas":canvas(),"id":node_id(9),"layer":"t1","set":{"x":10,"content":{"text":"Hello"}}}),
        "layers.remove" => json!({"canvas":canvas(),"id":node_id(9),"layer":"t1"}),
        "layers.reorder" => json!({"canvas":canvas(),"id":node_id(9),"layer":"t1","index":0}),
        "layers.align" => json!({"canvas":canvas(),"id":node_id(9),"layers":["t1"],"to":"center"}),
        "layers.font_add" => json!({"canvas":canvas(),"id":node_id(9),"font":{"hash":"00".repeat(32),"mime":"font/ttf"}}),
        "layers.font_remove" => json!({"canvas":canvas(),"id":node_id(9),"name":"Lato"}),
        "deck.font_add" => json!({"canvas":canvas(),"id":node_id(9),"font":{"hash":"00".repeat(32),"mime":"font/ttf"}}),
        "deck.font_remove" => json!({"canvas":canvas(),"id":node_id(9),"name":"Lato"}),
        "deck.create" => json!({"canvas":canvas(),"id":node_id(9),"pos":{"x":1,"y":2},"title":"Pitch","nodes":[node_id(1),node_id(2)]}),
        "deck.list" => json!({"canvas":canvas(),"id":node_id(9)}),
        "deck.set" => json!({"canvas":canvas(),"id":node_id(9),"slides":[{"id":"s1","name":"One","layers":[]},{"name":"Two"}]}),
        "deck.slide_add" => json!({"canvas":canvas(),"id":node_id(9),"slide":{"name":"Two"},"index":0}),
        "deck.slide_update" => json!({"canvas":canvas(),"id":node_id(9),"slide":"s1","name":"Renamed"}),
        "deck.slide_remove" => json!({"canvas":canvas(),"id":node_id(9),"slide":"s1"}),
        "deck.slide_move" => json!({"canvas":canvas(),"id":node_id(9),"slide":"s1","index":3}),
        "deck.layer_update" => json!({"canvas":canvas(),"id":node_id(9),"slide":"s1","layer":"t1","set":{"x":10,"content":{"text":"Hello"}}}),
        "deck.export" => json!({"canvas":canvas(),"id":node_id(9),"format":"pdf","path":"/nodecraft-panic-hunt/deck.pdf"}),
        "timeline.list" => json!({"canvas":canvas(),"id":node_id(9)}),
        "timeline.interchange" => json!({"canvas":canvas(),"id":node_id(9)}),
        "timeline.export_interchange" => json!({"canvas":canvas(),"id":node_id(9),"path":"/nodecraft-panic-hunt/cut.zip"}),
        "node.timeline_set" => json!({"canvas":canvas(),"id":node_id(9),"clips":[{"id":"t1","origin":"n1","out_us":1_000_000}]}),
        "timeline.update" => json!({"canvas":canvas(),"id":node_id(9),"clip":"t1","set":{"fade_in_us":500_000,"out_us":null}}),
        "timeline.move" => json!({"canvas":canvas(),"id":node_id(9),"clip":"t1","index":0}),
        "timeline.split" => json!({"canvas":canvas(),"id":node_id(9),"clip":"t1","at_us":1_000_000}),
        "timeline.remove" => json!({"canvas":canvas(),"id":node_id(9),"clip":"t1"}),
        "timeline.track_add" => json!({"canvas":canvas(),"id":node_id(9),"kind":"audio"}),
        "timeline.track_remove" => json!({"canvas":canvas(),"id":node_id(9),"track":1}),
        "timeline.caption_add" => json!({"canvas":canvas(),"id":node_id(9),"text":"Hello","start_us":0,"end_us":500_000,"stroke_width":2}),
        "timeline.caption_update" => json!({"canvas":canvas(),"id":node_id(9),"caption":"c1","set":{"text":"Bye","position":"top"}}),
        "timeline.caption_remove" => json!({"canvas":canvas(),"id":node_id(9),"caption":"c1"}),
        "element.create" => json!({"canvas":canvas(),"id":node_id(6),"name":"hero","category":"character","guidelines":"red scarf",
            "from":[{"node":node_id(5),"port":"out"}],"edges":[nodecraft_types::EdgeId::from_bytes([0x76; 16])],"pos":{"x":1,"y":2}}),
        "element.sheet" => json!({"canvas":canvas(),"element":node_id(6),"id":node_id(7),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16]),nodecraft_types::EdgeId::from_bytes([0x78; 16])],"pos":{"x":1,"y":2}}),
        "element.list" => json!({"canvas":canvas()}),
        "element.save" => json!({"canvas":canvas(),"id":node_id(6),"replace":true,"shared":true}),
        "element.place" => json!({"canvas":canvas(),"name":"hero","id":node_id(7),"pos":{"x":1,"y":2},"as":"hero2"}),
        "element.update" => json!({"canvas":canvas(),"id":node_id(6),"seed":node_id(7)}),
        "library.elements" => json!({}),
        "shader.save" => json!({"canvas":canvas(),"id":node_id(0x5a),"name":"Glow"}),
        "shader.place" => json!({"canvas":canvas(),"name":"Glow","version":1,"id":node_id(0x5b),"pos":{"x":1,"y":2}}),
        "shader.update" => json!({"canvas":canvas(),"id":node_id(0x5a),"name":"glow"}),
        "library.shaders" => json!({}),
        "library.remove_shader" => json!({"name":"Glow","version":1}),
        "library.remove_element" => json!({"name":"hero"}),
        "library.rename_element" => json!({"name":"hero","to":"villain"}),
        "library.share_element" => json!({"name":"hero","shared":true}),
        // In a folder that doesn't exist, so nothing is written or read.
        "library.export_element" => json!({"name":"hero","path":std::env::temp_dir().join("nodecraft-hunt-absent").join("hero.ncelement")}),
        "library.import_element" => json!({"path":std::env::temp_dir().join("nodecraft-hunt-absent").join("hero.ncelement"),"rename":"villain"}),
        "style.list" => json!({"canvas":canvas(),"for":node_id(5)}),
        "style.card" => {
            json!({"canvas":canvas(),"style":node_id(6),"id":node_id(7),"edge":nodecraft_types::EdgeId::from_bytes([0x79; 16]),"pos":{"x":1,"y":2}})
        }
        "style.use" => json!({"canvas":canvas(),"style":node_id(6),"target":node_id(5),"edge":nodecraft_types::EdgeId::from_bytes([0x79; 16])}),
        "folder.fill" => json!({"canvas":canvas(),"id":node_id(5),"folder":"Shots","modality":"text",
            "files":[{"name":"a.txt","value":{"type":"text","value":"first"}},{"name":"b/c.txt","value":{"type":"text","value":"second"}}]}),
        "document.use_pages" => json!({"canvas":canvas(),"node":node_id(5),"first":2,"last":3}),
        "document.extract_page" => json!({"canvas":canvas(),"node":node_id(5),"page":1,"id":node_id(7),"at":{"x":400,"y":20}}),
        "document.extract_text" => json!({"canvas":canvas(),"node":node_id(5),"page":1,"region":[0.0,0.0,1.0,1.0],"id":node_id(7),"at":{"x":400,"y":20}}),
        "mesh.capture" => json!({"canvas":canvas(),"from":{"node":node_id(5),"port":"out"},"shot":"views","id":node_id(7),
            "edge":nodecraft_types::EdgeId::from_bytes([0x77; 16]),"pos":{"x":1,"y":2},"yaw":200.0,"pitch":95.0,"zoom":3.33,"style":"wireframe"}),
        "mesh.view_set" => json!({"canvas":canvas(),"id":node_id(5),"view":{"yaw":200.0,"pitch":45.0,"zoom":3.33,"style":"wireframe"}}),
        _ => panic!("missing valid template for {id}"),
    };
    (session, params)
}

fn connected_template(id: &str, feeding: bool) -> (Session, Value) {
    let (mut session, mut params) = template(id);
    if feeding {
        assert_eq!(id, "node.create_connected");
        let edge = common::edge();
        session.dispatch("edge.disconnect", &json!({"canvas":common::canvas(),"id":edge.id})).unwrap();
        params.as_object_mut().unwrap().remove("from");
        params["to"] = json!(edge.to);
        params["kind"] = json!("source.text");
    }
    (session, params)
}

#[test]
#[ignore = "valid command templates with individual field mutations"]
fn typed_template_panic_hunt() {
    let _cleanup = RemoveExportFolder::hold();
    let mut ids: Vec<_> = Session::new(common::project()).unwrap().registry().specs().map(|spec| (spec.id, false)).collect();
    ids.push(("node.create_connected", true));
    let mut successes = 0;
    for (id, feeding) in ids {
        let (_, valid) = connected_template(id, feeding);
        let mut cases = vec![valid.clone()];
        if let Some(object) = valid.as_object() {
            for key in object.keys() {
                for raw in [Value::Null, json!(true), json!("secret-do-not-echo"), json!(u64::MAX), json!([]), json!({})] {
                    let mut mutated = valid.clone();
                    mutated[key] = raw;
                    cases.push(mutated);
                }
                let mut missing = valid.clone();
                missing.as_object_mut().unwrap().remove(key);
                cases.push(missing);
            }
        }
        for path in [
            "/field/rect/pos/x",
            "/field/rect/width",
            "/delta/x",
            "/rect/pos/x",
            "/pos/x",
            "/pos/y",
            "/node/id",
            "/node/kind",
            "/node/pos/x",
            "/edge/from/node",
            "/edge/from/port",
            "/edge/to/node",
            "/edge/order",
            "/from/node",
            "/from/port",
            "/to/node",
            "/to/port",
            "/tag/id",
            "/tag/title",
            "/tag/color",
            "/nodes/0",
            "/ids/0",
            "/ids/1",
            "/tags/0",
            "/rows/0",
            "/rows/1",
            "/prompts/0",
        ] {
            if valid.pointer(path).is_some() {
                for raw in [Value::Null, json!("secret-do-not-echo"), json!(1e100), json!([])] {
                    let mut mutated = valid.clone();
                    *mutated.pointer_mut(path).unwrap() = raw;
                    cases.push(mutated);
                }
            }
        }
        for mut params in cases {
            let (mut session, fresh) = connected_template(id, feeding);
            // Keep the valid token bound to this exact template Session; preserve deliberately
            // mutated/missing tokens and all other mutations for the adversarial cases.
            if id == "run.start" && params.pointer("/accept/token") == valid.pointer("/accept/token") {
                params["accept"]["token"] = fresh["accept"]["token"].clone();
            }
            if id == "node.set_model" && params.get("preview_binding") == valid.get("preview_binding") {
                params["preview_binding"] = fresh["preview_binding"].clone();
            }
            if matches!(id, "plugins.result" | "plugins.forget") && params.get("pending") == valid.get("pending") {
                params["pending"] = fresh["pending"].clone();
            }
            let before = session.document().clone();
            let revision = session.revision();
            let events = session.events_since(0).unwrap();
            let history = (session.can_undo(), session.can_redo());
            let result = session.dispatch(id, &params);
            if feeding && params == valid {
                assert!(result.is_ok(), "the valid upstream-creation template must reach the new command path: {result:?}");
            }
            // The font commands need a font file the template's project doesn't hold; their own
            // tests cover a valid add and remove.
            let needs_font = matches!(id, "layers.font_add" | "layers.font_remove");
            if (id.starts_with("layers.") || id == "node.layers_set") && params == valid && !needs_font {
                assert!(result.is_ok(), "the valid {id} template reaches the layer code: {result:?}");
            }
            // The interchange commands need media the template's Timeline doesn't have; their
            // own tests cover a valid export.
            let needs_media = matches!(id, "timeline.interchange" | "timeline.export_interchange");
            if (id.starts_with("timeline.") || id == "node.timeline_set") && params == valid && !needs_media {
                assert!(result.is_ok(), "the valid {id} template reaches the clip code: {result:?}");
            }
            match result {
                Err(error) => {
                    assert_ne!(error.kind, ErrorKind::Internal, "{id}");
                    assert!(!error.message.contains("secret-do-not-echo"), "{id}: {}", error.message);
                    assert_eq!(session.document(), &before);
                    assert_eq!(session.revision(), revision);
                    assert_eq!(session.events_since(0).unwrap(), events);
                }
                Ok(_) => {
                    successes += 1;
                    if matches!(
                        id,
                        "canvas.fit_all"
                            | "canvas.fit_selection"
                            | "canvas.minimap_toggle" | "canvas.prompt_overlays_toggle"
                            | "canvas.pan_to"
                            | "canvas.tag_filter_open"
                            | "canvas.tag_filter_close"
                            | "canvas.tag_filter_set"
                            | "canvas.tag_filter_next"
                            | "canvas.tag_filter_previous"
                            | "canvas.comments_toggle"
                            | "comment.open"
                            | "comment.compose"
                            | "canvas.stale"
                            | "sheet.list"
                            | "sheet.rows"
                            | "node.describe"
                            | "node.options"
                            | "options.refresh"
                            | "options.status"
                            | "comfy.nodes.refresh"
                            | "comfy.nodes.status"
                            | "app.check_updates"
                            | "app.update_status"
                            | "app.install_update"
                            | "app.restart_to_update"
                            | "node.model_preview"
                            | "canvas.search_open"
                            | "canvas.search_query"
                            | "canvas.search_next"
                            | "canvas.search_previous"
                            | "canvas.search_close"
                            | "run.quote"
                            | "run.estimate"
                            // Applies cached results first (#91); the fixture has none to reuse.
                            | "run.start"
                            | "usage.export_csv"
                            | "perf.start"
                            | "perf.stop"
                            | "perf.clear"
                            | "perf.report"
                            | "usage.ownership"
                            | "usage.reconcile_ownership"
                            | "usage.unsettled"
                            | "usage.resolve_charge"
                            // The fixture has no results to reuse: nothing changes.
                            | "run.reuse"
                    ) {
                        assert_eq!(session.document(), &before);
                        assert_eq!(session.revision(), revision);
                        assert_eq!(session.events_since(0).unwrap(), events);
                        assert_eq!((session.can_undo(), session.can_redo()), history);
                        continue;
                    }
                    // A command that found nothing to change (asking for the value it has) edits nothing.
                    if session.revision() == revision {
                        assert_eq!(session.document(), &before, "{id}");
                        assert_eq!(session.events_since(0).unwrap(), events, "{id}");
                        assert_eq!((session.can_undo(), session.can_redo()), history, "{id}");
                        continue;
                    }
                    let after = session.document().clone();
                    session.document().validate().unwrap();
                    if id == "canvas.paste" {
                        assert_ne!(after, before, "a successful paste is a document edit");
                        assert_eq!(session.revision(), revision + 1);
                    }
                    if id.starts_with("file.") || id.starts_with("export.") {
                        // File and export commands write outside the project; neither edits it.
                        assert_eq!(after, before);
                        continue;
                    }
                    let (reverse, again) = if id == "edit.undo" { ("edit.redo", "edit.undo") } else { ("edit.undo", "edit.redo") };
                    session.dispatch(reverse, &Value::Null).unwrap();
                    assert_eq!(session.document(), &before, "{id}");
                    session.dispatch(again, &Value::Null).unwrap();
                    assert_eq!(session.document(), &after, "{id}");
                }
            }
        }
    }
    assert!(successes >= 7);
}

#[test]
fn run_start_template_reaches_the_registered_host() {
    let (mut session, params) = template("run.start");
    let before = session.document().clone();
    let events = session.events_since(0).unwrap();
    let revision = session.revision();
    let history = (session.can_undo(), session.can_redo());
    assert_eq!(session.dispatch("run.start", &params).unwrap(), json!({"run":"accepted"}));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.events_since(0).unwrap(), events);
    assert_eq!((session.can_undo(), session.can_redo()), history);
}

#[test]
fn model_templates_reach_the_production_kind_and_current_preview_binding() {
    for id in ["node.model_preview", "node.set_model"] {
        let (mut session, params) = template(id);
        let before = session.document().clone();
        session.dispatch(id, &params).unwrap();
        if id == "node.model_preview" {
            assert_eq!(session.document(), &before);
        } else {
            assert_ne!(session.document(), &before);
        }
    }
}

#[test]
fn plugin_templates_reach_the_real_store_and_results_without_document_edits() {
    for id in [
        "plugins.list",
        "plugins.import",
        "plugins.update",
        "plugins.reload",
        "plugins.enable",
        "plugins.disable",
        "plugins.rollback",
        "plugins.uninstall",
        "plugins.rights",
        "plugins.prune",
        "plugins.result",
        "plugins.forget",
    ] {
        let (mut session, params) = template(id);
        let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.can_undo(), session.can_redo());
        let result = session.dispatch(id, &params).unwrap();
        if id == "plugins.list" {
            assert_eq!(result["packages"][0]["version"], "1.1.0");
        }
        if id == "plugins.import" {
            assert_eq!(result["package_id"], "io.example.other");
            assert_eq!(session.dispatch(id, &json!({"location":"malformed"})).unwrap_err().kind, ErrorKind::InvalidInput);
        }
        if id == "plugins.enable" {
            assert_eq!(result["registered"], json!(["plugin.io.example.palette.warm"]));
        }
        if id == "plugins.reload" {
            assert_eq!((&result["version"], &result["replaced"].is_string()), (&json!("1.2.0"), &true));
            let again = session.dispatch(id, &params).unwrap_err();
            assert_eq!(again.kind, ErrorKind::Conflict, "an unchanged file isn't reloaded: {again:?}");
        }
        if id == "plugins.result" {
            assert_eq!(result["status"], "completed");
        }
        if id == "plugins.forget" {
            assert_eq!(result["forgotten"], true);
        }
        assert_eq!((session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.can_undo(), session.can_redo()), before, "{id}");
    }
}

/// The legacy fixture with its one quarantined group reconciled.
fn reconciled_legacy() -> nodecraft_exec::cost::UsageLog {
    let mut log: nodecraft_exec::cost::UsageLog =
        serde_json::from_str(include_str!("../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    let token: String = log.ownership_group("a", nodecraft_types::Currency::USD).unwrap().token.iter().map(|b| format!("{b:02x}")).collect();
    nodecraft_engine::ownership_cmds::reconcile(&mut log, &json!({"session":"a","currency":"USD","token":token,"holder":"named"}), 0).unwrap();
    log
}

#[test]
fn template_fetch_corpus_reaches_shared_commands_without_project_mutation() {
    for id in ["template.refresh", "template.retrieve"] {
        let (mut session, params) = template(id);
        let before = session.document().clone();
        let revision = session.revision();
        let result = session.dispatch(id, &params).unwrap();
        if id == "template.refresh" {
            assert_eq!(result, json!({"pending":1}));
        } else {
            assert_eq!(result, json!({"available":true}));
        }
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
    }
}
