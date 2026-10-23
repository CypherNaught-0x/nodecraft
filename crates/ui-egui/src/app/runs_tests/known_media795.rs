//! Public-safe run-review evidence for a deferred PDF reader; no provider sends or keychain.
use super::*;
use nodecraft_types::{AssetRef, MediaMeta};

const WRITER: NodeId = NodeId::from_bytes([94; 16]);
const PDF: NodeId = NodeId::from_bytes([95; 16]);
const CATALOG: &str = r#"
[[model]]
id = "fixture.writer"
provider = "mock"
provider_model = "mock-text"
label = "Prompt writer"
description = "Public deterministic fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "fixture.reader"
provider = "mock"
provider_model = "mock-text"
label = "PDF reader"
description = "Public deterministic fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.modes]]
id = "document-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "document", modality = "document", min = 1, max = 5 }]
document = { role = "document", max_pages = 100, tokens_per_page = 1000 }
"#;

fn scene() -> (Harness<'static, NodeCraftApp>, MockProvider) {
    let snapshot = GenerationCatalog::new(RunCatalog::new(nodecraft_catalog::Catalog::from_toml(CATALOG).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([95; 16]), "Known media limits").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for (id, model, title, x, y) in [(WRITER, "fixture.writer", "Prompt writer", 60.0, 130.0), (N, "fixture.reader", "PDF reader", 730.0, 180.0)] {
        let mut node = kinds
            .get(nodecraft_nodes::generation::TEXT_KIND)
            .unwrap()
            .instantiate_instance(
                id,
                Default::default(),
                &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("Write a concise report summary".into()))].into(),
            )
            .unwrap();
        node.title = title.into();
        node.pos = nodecraft_graph::Position { x, y };
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mock = MockProvider::standard();
    let mut services = Services::default();
    let runs = Runs::with_maker(&mut services, making(mock.clone())).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    runs.install(&mut session);
    let hash = session.import_asset(b"%PDF-1.7\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n").unwrap();
    let asset = AssetRef { hash, mime: "application/pdf".parse().unwrap(), meta: MediaMeta { page_count: Some(300), ..Default::default() } };
    let mut source = session
        .kinds()
        .get(nodecraft_nodes::document::SOURCE)
        .unwrap()
        .instantiate_instance(PDF, Default::default(), &[("asset".into(), Value::Media(asset))].into())
        .unwrap();
    source.title = "300-page report".into();
    source.pos = nodecraft_graph::Position { x: 60.0, y: 470.0 };
    session.dispatch("node.add", &json!({"canvas":C,"node":source})).unwrap();
    for (n, from, port) in [(10, WRITER, "prompt"), (11, PDF, "document")] {
        let edge = json!({"id":nodecraft_types::EdgeId::from_bytes([n;16]),"from":{"node":from,"port":"out"},"to":{"node":N,"port":port},"order":0});
        session.dispatch("edge.connect", &json!({"canvas":C,"edge":edge})).unwrap();
    }
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    app.selection.only(N);
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    // This is the same review entry point used by running checked Sheet rows together.
    h.state_mut().request_runs(vec![WRITER, N], super::super::generate::RunKind::Generate).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some());
    (h, mock)
}

#[test]
fn a_deferred_pdf_reader_shows_the_known_refusal_before_a_paid_confirmation_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let (mut h, mock) = scene();
        h.state_mut().theme = theme;
        crate::theme::apply(&h.ctx, theme);
        h.run();
        h.get_by_label_contains("this model reads at most 100");
        assert!(h.query_by_label_contains("Generate for").is_none());
        assert!(mock.requests().is_empty());
        press(&mut h, Role::Button, "Cancel");
        assert!(h.state().run_confirm.is_none());
        assert!(mock.requests().is_empty());
    }
}

#[test]
#[ignore = "manual public-safe GPU evidence; KNOWN_MEDIA795_RENDER_DIR"]
fn known_media795_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("KNOWN_MEDIA795_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let allowed = std::env::var_os("KNOWN_MEDIA795_EXPECT_ALLOWED").is_some();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "dark"), (crate::theme::ThemeKind::Light, "light")] {
        let (h, mock) = scene();
        let mut app = h.into_state();
        app.theme = theme;
        let mut shot =
            Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        if allowed {
            shot.get_by_label_contains("Generate for");
        } else {
            shot.get_by_label_contains("this model reads at most 100");
        }
        assert!(mock.requests().is_empty());
        shot.render().unwrap().save(dir.join(format!("{}-{name}.png", if allowed { "before" } else { "after" }))).unwrap();
    }
}
