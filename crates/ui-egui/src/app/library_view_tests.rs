//! The Techniques panel (#96): place a project's version, keep it in the library, and place it
//! from the library in another project.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::library::MemoryLibrary;
use nodecraft_types::TechniqueId;

const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const T: TechniqueId = TechniqueId::from_bytes([0x7e; 16]);

fn harness_on(store: &MemoryLibrary) -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless_with_library(Box::new(store.clone())).unwrap())
}

/// A project on `store` with its text node published as V1 of technique "Echo".
fn publishing(store: &MemoryLibrary) -> Harness<'static, NodeCraftApp> {
    let mut h = harness_on(store);
    let app = h.state_mut();
    let canvas = app.canvas;
    let text = app.session.document().canvases().get(&canvas).unwrap().nodes().values().find(|n| n.kind == "source.text").unwrap().id;
    let params = json!({"canvas": canvas, "published": V1, "seed": NodeId::from_bytes([40; 16]), "technique": T,
        "name": "Echo", "outputs": [{"node": text, "name": "Text"}]});
    app.session.dispatch("technique.publish", &params).unwrap();
    h.run();
    h
}

fn open_panel(h: &mut Harness<'static, NodeCraftApp>) {
    crate::app::topbar_tests::reveal(h, "Techniques");
    h.get_by_label("Techniques").click();
    h.run();
    assert!(h.state().library.open);
}

/// The technique nodes on the open canvas.
fn placed(h: &Harness<'static, NodeCraftApp>) -> Vec<Node> {
    let app = h.state();
    app.session.document().canvases().get(&app.canvas).unwrap().nodes().values().filter(|n| n.kind == nodecraft_nodes::technique::ID).cloned().collect()
}

#[test]
fn a_project_version_is_placed_in_the_middle_of_the_view_and_selected() {
    let mut h = publishing(&MemoryLibrary::default());
    open_panel(&mut h);
    assert!(h.query_by_label("Echo · v1").is_some());
    assert!(h.query_by_label_contains("The library is empty").is_some());
    let before = h.state().session.document().clone();
    let expected = h.state().view.to_world(h.state().viewport.min, h.state().navigation.usable(h.state().viewport, h.state().theme.tokens()).center());
    h.get_by_label("Place").click();
    h.run();
    let nodes = placed(&h);
    assert_eq!(nodes.len(), 1, "{:?}", h.state().notice);
    assert!(h.state().selection.contains(nodes[0].id));
    // Local evaluation may finish during the pointer-driven frame and append a history
    // strip. Placement centers the fresh command payload, before that later graph edit.
    let mut created = nodes[0].clone();
    created.history.clear();
    created.active = None;
    let rect = payload_rect(&h.state().session, &created, h.state().theme.tokens());
    assert!(
        (rect.center() - expected).length() < 0.01,
        "resolved library card geometry must center in view: actual {:?}, expected {:?}, viewport {:?}, view {:?}",
        rect.center(),
        expected,
        h.state().viewport,
        h.state().view
    );
    assert_eq!(nodecraft_nodes::technique::interface(&nodes[0]).unwrap().canvas, V1);
    let app = h.state_mut();
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(app.session.document(), &before, "placing is one undo step");
}

#[test]
fn a_version_kept_in_the_library_is_placed_in_another_project_and_outlives_removal() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    open_panel(&mut a);
    a.get_by_label("Add to library").click();
    a.run();
    assert!(a.query_by_label("In library").is_some(), "{:?}", a.state().notice);
    assert_eq!(store.saved().unwrap().versions().count(), 1);

    let mut b = harness_on(&store);
    open_panel(&mut b);
    assert!(b.query_by_label_contains("Nothing published yet").is_some());
    assert!(b.query_by_label("Echo · v1").is_some(), "the library lists A's version");
    b.get_by_label("Place").click();
    b.run();
    let nodes = placed(&b);
    assert_eq!(nodes.len(), 1, "{:?}", b.state().notice);
    let copy = nodecraft_nodes::technique::interface(&nodes[0]).unwrap().canvas;
    let info = b.state().session.document().canvases().get(&copy).unwrap().technique_info().unwrap().clone();
    assert_eq!((info.id, info.version), (T, 1));
    assert_ne!(copy, V1, "the copy takes a new id");
    // Now the project has it too, beside the library's entry.
    assert_eq!(b.get_all_by_label("Echo · v1").count(), 2);

    b.get_by_label("Remove").click();
    b.run();
    assert!(b.query_by_label_contains("The library is empty").is_some(), "{:?}", b.state().notice);
    assert_eq!(store.saved().unwrap().versions().count(), 0);
    assert_eq!(placed(&b).len(), 1, "the project keeps its copy");
    assert!(b.state().session.document().canvases().contains_key(&copy));
}

#[test]
fn a_library_shader_is_placed_as_a_card_and_removed() {
    let mut h = harness_on(&MemoryLibrary::default());
    let app = h.state_mut();
    let (canvas, card) = (app.canvas, NodeId::from_bytes([0x5a; 16]));
    app.session.dispatch("node.create", &json!({"canvas": canvas, "id": card, "kind": nodecraft_nodes::shader::ID, "pos": {"x": 0, "y": 0}})).unwrap();
    app.session.dispatch("shader.save", &json!({"canvas": canvas, "id": card, "name": "Glow"})).unwrap();
    h.run();
    open_panel(&mut h);
    assert!(h.query_by_label("Glow · v1").is_some());
    h.get_by_label("Place").click();
    h.run();
    let app = h.state();
    let shaders: Vec<&Node> = app
        .session
        .document()
        .canvases()
        .get(&app.canvas)
        .unwrap()
        .nodes()
        .values()
        .filter(|n| n.kind == nodecraft_nodes::shader::ID && n.id != card)
        .collect();
    assert_eq!(shaders.len(), 1, "{:?}", app.notice);
    assert_eq!(shaders[0].title, "Glow");
    assert_eq!(nodecraft_nodes::shader::pinned(shaders[0]).map(|pin| pin.version), Some(1));
    assert!(app.selection.contains(shaders[0].id));
    h.get_by_label("Remove").click();
    h.run();
    assert!(h.query_by_label_contains("No shaders saved yet").is_some(), "{:?}", h.state().notice);
}

#[test]
#[ignore = "manual public-safe capture; LIBRARY96_RENDER_DIR"]
fn library96_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("LIBRARY96_RENDER_DIR").unwrap());
    let store = MemoryLibrary::default();
    let mut h = publishing(&store);
    open_panel(&mut h);
    h.get_by_label("Add to library").click();
    h.run();
    h.get_all_by_label("Place").next().unwrap().click();
    h.run();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "library96-dark.png"), (crate::theme::ThemeKind::Light, "library96-light.png")] {
        h.state_mut().theme = theme;
        let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
}
