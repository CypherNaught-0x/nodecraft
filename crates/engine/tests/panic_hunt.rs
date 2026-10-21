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
    if id == "mesh.capture" {
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
    if id.starts_with("timeline.") || id == "node.timeline_set" {
        // A Timeline (node 9) holding a clip `t1` with a known end and an empty audio track 1, so the valid templates reach
        // the clip code rather than the kind check.
        let mut editor = nodecraft_nodes::timeline::kind().descriptor().instantiate(node_id(9), &std::collections::BTreeMap::new()).unwrap();
        let stored = json!({"clips": [{"id":"t1","origin":"n1","in_us":0,"out_us":2_000_000}], "tracks": [{"kind":"audio"}]});
        editor.config.insert(nodecraft_nodes::timeline::CONFIG.into(), nodecraft_types::Value::Text(stored.to_string()));
        nodecraft_graph::Change::AddNode { canvas: canvas(), node: editor }.apply(&mut document).unwrap();
    }
    let root = nodecraft_types::CommentId::from_bytes([10; 16]);
    let reply = nodecraft_types::CommentId::from_bytes([11; 16]);
    if id.starts_with("comment.") {
        let mut comment = nodecraft_graph::Comment::new(root, "Original", nodecraft_graph::Position::default());
        comment.replies.push(nodecraft_graph::CommentReply { id: reply, text: "Reply".into() });
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
    if matches!(id, "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" | "canvas.pan_to") {
        session.swap_resource(Some(nodecraft_engine::navigation_cmds::NavigationContext {
            canvas: canvas(),
            viewport_available: true,
            blocked: false,
            fit_all: true,
            fit_selection: true,
        }));
        session.register_host_service(nodecraft_engine::navigation_cmds::SERVICE, |_| Ok(json!({"accepted":true}))).unwrap();
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
    if id.starts_with("run.") || id == "usage.export_csv" {
        use nodecraft_engine::run_cmds::*;
        session.swap_resource(Some(RunCatalog::new(nodecraft_catalog::Catalog::builtin().unwrap()).unwrap()));
        session.swap_resource(Some(BudgetState { budgets: Default::default(), spent: Default::default() }));
        session.swap_resource(Some(UsageSnapshot(std::sync::Arc::new(Default::default()))));
        session.register_run_host(|_| Ok(json!({"run":"accepted"}))).unwrap();
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
    let plugin_old =
        if id.starts_with("plugins.") && !matches!(id, "plugins.result" | "plugins.forget") { Some(plugins::register(&mut session)) } else { None };
    let pending = if matches!(id, "plugins.result" | "plugins.forget") { Some(plugins::register_results(&mut session)) } else { None };
    let movement = json!({"canvas":canvas(),"id":node_id(1),"pos":{"x":12,"y":0}});
    let params = match id {
        "tag.put" => json!({"canvas":canvas(),"tag":{"id":nodecraft_types::TagId::from_bytes([9;16]),"title":"After","color":"blue"}}),
        "tag.remove" => json!({"canvas":canvas(),"tag":nodecraft_types::TagId::from_bytes([9;16])}),
        "node.tag_set" => json!({"canvas":canvas(),"nodes":[node_id(1)],"tag":null}),
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
        // The updates service isn't registered here: the check must be refused cleanly.
        "app.check_updates" | "app.update_status" | "app.install_update" | "app.restart_to_update" => json!({}),
        "edge.rewire" => {
            let moved = nodecraft_graph::Edge { id: nodecraft_types::EdgeId::from_bytes([9; 16]), ..edge() };
            json!({"canvas":canvas(),"edge":moved,"replace":[edge().id]})
        }
        "canvas.paste" => json!({"canvas":canvas(),"nodes":[node(3)],"edges":[]}),
        "run.quote" => json!({"canvas":canvas(),"mode":"selected","nodes":[node_id(1)]}),
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
        // A text result never writes, so the hunt leaves no files behind.
        "history.export" => json!({"canvas":canvas(),"id":node_id(1),"generation":gen_id(0x21),"path":"/nodecraft-panic-hunt/out.png"}),
        "node.create" => json!({"canvas":canvas(),"id":node_id(7),"kind":"source.text","pos":{"x":1,"y":2}}),
        "node.create_connected" => json!({"canvas":canvas(),"id":node_id(7),"kind":"structure.router","pos":{"x":1,"y":2},
            "from":{"node":node_id(1),"port":"out"},"edge":nodecraft_types::EdgeId::from_bytes([0x77; 16])}),
        "canvas.delete" => json!({"canvas":canvas(),"nodes":[node_id(1)],"edges":[edge().id]}),
        "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" => json!({}),
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
        "keys.remove" | "keys.status" => json!({"account":"openai"}),
        "file.open" => json!({"location":"template.ncraft","discard_dirty":true}),
        "file.save" => Value::Null,
        "file.save_as" => json!({"location":"new.ncraft"}),
        "file.cancel" => json!({}),
        // Real memory store and private package files, never caller-selected filesystem paths.
        "plugins.list" | "plugins.prune" => json!({}),
        "plugins.import" | "plugins.update" => json!({"location":"palette.ncraft-plugin","dry_run":true}),
        "plugins.enable" | "plugins.disable" | "plugins.uninstall" => json!({"package_id":"io.example.palette"}),
        "plugins.rollback" => json!({"package_id":"io.example.palette","digest":plugin_old.unwrap()}),
        "plugins.result" | "plugins.forget" => json!({"pending":pending.unwrap()}),
        "run.reuse" => json!({"canvas":canvas()}),
        "run.held" => json!({}),
        "run.held_retry" | "run.held_place" | "run.held_discard" => json!({"id":1}),
        "node.shader_set" => json!({"canvas":canvas(),"id":node_id(1),"source":"fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }"}),
        "node.cache_set" => json!({"canvas":canvas(),"id":node_id(1),"enabled":true}),
        "node.mask_set" => json!({"canvas":canvas(),"id":node_id(1),"strokes":[{"size":0.1,"hardness":0.5,"points":[[0.5,0.5]]}]}),
        "mask.create" => json!({"canvas":canvas(),"from":{"node":node_id(1),"port":"out"},"expand":node_id(7),"mask":node_id(8),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16]),nodecraft_types::EdgeId::from_bytes([0x78; 16])],"pos":{"x":1,"y":2}}),
        "mask.margins_set" => json!({"canvas":canvas(),"id":node_id(1),"margins":{"left":0,"top":0,"right":64,"bottom":0}}),
        "mask.edit" => json!({"canvas":canvas(),"mask":node_id(1),"id":node_id(7),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16]),nodecraft_types::EdgeId::from_bytes([0x78; 16])],"pos":{"x":1,"y":2},"prompt":"a door"}),
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
        "element.create" => json!({"canvas":canvas(),"id":node_id(6),"name":"hero","category":"character","guidelines":"red scarf",
            "from":[{"node":node_id(5),"port":"out"}],"edges":[nodecraft_types::EdgeId::from_bytes([0x76; 16])],"pos":{"x":1,"y":2}}),
        "element.sheet" => json!({"canvas":canvas(),"element":node_id(6),"id":node_id(7),
            "edges":[nodecraft_types::EdgeId::from_bytes([0x77; 16]),nodecraft_types::EdgeId::from_bytes([0x78; 16])],"pos":{"x":1,"y":2}}),
        "element.list" => json!({"canvas":canvas()}),
        "document.use_pages" => json!({"canvas":canvas(),"node":node_id(5),"first":2,"last":3}),
        "mesh.capture" => json!({"canvas":canvas(),"from":{"node":node_id(5),"port":"out"},"shot":"views","id":node_id(7),
            "edge":nodecraft_types::EdgeId::from_bytes([0x77; 16]),"pos":{"x":1,"y":2},"yaw":200.0,"pitch":95.0,"zoom":3.33,"style":"wireframe"}),
        _ => panic!("missing valid template for {id}"),
    };
    (session, params)
}

#[test]
#[ignore = "valid command templates with individual field mutations"]
fn typed_template_panic_hunt() {
    let ids: Vec<_> = Session::new(common::project()).unwrap().registry().specs().map(|spec| spec.id).collect();
    let mut successes = 0;
    for id in ids {
        let (_, valid) = template(id);
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
            "/pos/x",
            "/pos/y",
            "/node/id",
            "/node/kind",
            "/node/pos/x",
            "/edge/from/node",
            "/edge/from/port",
            "/edge/to/node",
            "/edge/order",
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
            let (mut session, fresh) = template(id);
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
            if (id.starts_with("layers.") || id == "node.layers_set") && params == valid {
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
                            | "canvas.minimap_toggle"
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
                    if id.starts_with("file.") {
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
        "plugins.enable",
        "plugins.disable",
        "plugins.rollback",
        "plugins.uninstall",
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
