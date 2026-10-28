//! Real card previews and original SVG history in both themes. All artwork is authored here.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_graph::Generation;
use nodecraft_types::GenerationId;

const SVG: &[u8]=br##"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="200" viewBox="0 0 320 200"><rect width="320" height="200" rx="20" fill="#242641"/><path d="M55 145 L130 35 L200 145 Z" fill="#8b7af1"/><circle cx="230" cy="75" r="40" fill="#5de4be"/><path d="M35 170 Q160 115 285 170" fill="none" stroke="#ffffff" stroke-width="5"/></svg>"##;
const VECTOR: NodeId = NodeId::from_bytes([35; 16]);

fn scene(builder: egui_kittest::HarnessBuilder<NodeCraftApp>, theme: ThemeKind) -> Harness<'static, NodeCraftApp> {
    let mut project = Project::new(ProjectId::from_bytes([35; 16]), "Public vector study").unwrap();
    let canvas = CanvasId::from_bytes([35; 16]);
    Change::AddCanvas { canvas: Canvas::new(canvas, "Vector canvas") }.apply(&mut project).unwrap();
    let catalog = nodecraft_engine::generation_catalog::GenerationCatalog::builtin().unwrap();
    let kinds = catalog.registry().unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(catalog.clone());
    resources.insert(catalog.run_catalog());
    let mut session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    import_files(&mut session, canvas, nodecraft_graph::Position { x: -280.0, y: -100.0 }, 240.0, &[Incoming { name: "original.svg", bytes: SVG }], &[VECTOR])
        .unwrap();
    let asset = nodecraft_engine::split::preview_image_of(session.document().canvases().get(&canvas).unwrap().nodes().get(&VECTOR).unwrap()).unwrap().clone();
    let values =
        [("model".into(), Value::Text("fal.recraft41-vector".into())), ("prompt".into(), Value::Text("An authored geometric SVG illustration".into()))].into();
    let mut node = session
        .kinds()
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(NodeId::from_bytes([36; 16]), Default::default(), &values)
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 30.0, y: -100.0 };
    node.title = "SVG generation history".into();
    let id = GenerationId::from_bytes([35; 16]);
    node.history.push(Generation::new(id, vec![Value::List(vec![Value::Media(asset)])]));
    node.active = Some(id);
    let mut project = session.document().clone();
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(catalog.clone());
    resources.insert(catalog.run_catalog());
    session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), catalog.registry().unwrap())
        .unwrap()
        .with_resources(resources);
    session.import_asset(SVG).unwrap();
    let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
    app.theme = theme;
    let mut h = builder.with_size(Vec2::new(1100.0, 760.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    theme::apply(&h.ctx, theme);
    for _ in 0..200 {
        h.run();
        if h.query_all_by_label("Image 320×200").count() >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h
}

#[test]
fn recraft35_svg_source_and_history_previews_render_in_both_themes() {
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = scene(Harness::builder(), theme);
        assert!(h.query_all_by_label("Image 320×200").count() >= 2);
        assert!(h.state().previews.bytes() > 0);
        assert_eq!(crate::history::suggested_name("image/svg+xml"), "result.svg");
        let before = h.state().previews.bytes();
        for _ in 0..10 {
            h.run();
        }
        assert_eq!(h.state().previews.bytes(), before);
        // Pointer selection still uses the ordinary source card, whose original remains SVG.
        h.query_all_by_label("Image 320×200").next().unwrap().click();
        h.run();
        let asset =
            nodecraft_engine::split::preview_image_of(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&VECTOR).unwrap())
                .unwrap();
        assert_eq!(asset.mime.essence(), "image/svg+xml");
        h.state_mut().selection.only(NodeId::from_bytes([36; 16]));
        for _ in 0..8 {
            h.step();
        }
        h.get_by_label("Expand").click();
        h.run();
        assert_eq!(h.state().viewer, Some(NodeId::from_bytes([36; 16])));
        assert!(h.query_all_by_label("Image").count() >= 2, "SVG history opens the ordinary viewer beside the source Image label");
    }
}

#[test]
#[ignore = "manual GPU evidence; NODECRAFT_RECRAFT35_RENDER_ROOT"]
fn recraft35_render_source_and_history() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_RECRAFT35_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = scene(Harness::builder().wgpu(), theme);
        h.hover_at(egui::pos2(1090.0, 750.0));
        h.run();
        assert!(h.query_all_by_label("Image 320×200").count() >= 2);
        h.render().unwrap().save(root.join(format!("{theme:?}-after.png"))).unwrap();
    }
}
