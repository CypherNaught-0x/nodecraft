//! The PDF reader (#590): paging with keys and clicks, extracting a page or its text into new
//! nodes, and setting a PDF to images node's range, each one undo step.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_types::Unsigned;

const PDF: NodeId = NodeId::from_bytes([70; 16]);
const PAGES: NodeId = NodeId::from_bytes([71; 16]);

/// The headless app holding a three-page PDF source.
fn with_pdf() -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let bytes = nodecraft_media::pdf_fixture::pdf(&[
        "BT /F1 12 Tf 20 150 Td (Cover) Tj ET",
        "BT /F1 12 Tf 20 150 Td (Middle page) Tj ET",
        "BT /F1 12 Tf 20 150 Td (The end) Tj ET",
    ]);
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "paper.pdf", bytes: &bytes }], &[PDF]).unwrap();
    h.run();
    h
}

fn page(h: &Harness<'static, NodeCraftApp>) -> Option<u32> {
    h.state().reader.as_ref().map(|reader| reader.page)
}

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}

#[test]
fn the_reader_pages_with_keys_and_buttons_without_editing_the_document() {
    let mut h = with_pdf();
    let revision = h.state().session.revision();
    h.state_mut().open_reader(PDF);
    h.run();
    assert_eq!(page(&h), Some(0));
    assert!(h.query_by_label_contains("page 1 of 3").is_some());
    h.key_press(egui::Key::ArrowRight);
    h.run();
    assert_eq!(page(&h), Some(1));
    h.key_press(egui::Key::End);
    h.run();
    assert_eq!(page(&h), Some(2));
    h.key_press(egui::Key::PageDown);
    h.run();
    assert_eq!(page(&h), Some(2), "past the end stays on the last page");
    h.key_press(egui::Key::Home);
    h.run();
    assert_eq!(page(&h), Some(0));
    h.get_by_label("Next").click();
    h.run();
    assert_eq!(page(&h), Some(1));
    h.get_by_label("Previous").click();
    h.run();
    assert_eq!(page(&h), Some(0));
    assert_eq!(h.state().session.revision(), revision, "paging is view state");
}

#[test]
fn a_page_and_its_text_become_new_nodes_each_one_undo_step() {
    let mut h = with_pdf();
    h.state_mut().open_reader(PDF);
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.run();
    let before = nodes(&h).len();
    h.get_by_label("New Text node from page").click();
    h.run();
    let all = nodes(&h);
    assert_eq!(all.len(), before + 1, "{:?}", h.state().notice);
    // The new node is selected.
    let [made] = h.state().selection.nodes().iter().copied().collect::<Vec<_>>()[..] else { panic!("one node selected") };
    let text = all.iter().find(|node| node.id == made).unwrap();
    assert_eq!(text.kind, "source.text");
    assert_eq!(text.params.get("text"), Some(&Value::Text("Middle page".into())));
    h.get_by_label("Extract page").click();
    h.run();
    let all = nodes(&h);
    let copy = all.iter().find(|node| node.kind == nodecraft_nodes::document::SOURCE && node.id != PDF).unwrap();
    let Some(Value::Media(asset)) = copy.params.get("asset") else { panic!("an asset") };
    assert_eq!(asset.meta.page_count, Some(1));
    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(nodes(&h).len(), before);
}

#[test]
fn opened_from_pdf_to_images_it_sets_the_page_range_undoably() {
    let mut h = with_pdf();
    let canvas = h.state().canvas;
    let action =
        h.state().session.kinds().get(nodecraft_nodes::document::PAGES).unwrap().instantiate_instance(PAGES, Default::default(), &Default::default()).unwrap();
    let s = &mut h.state_mut().session;
    s.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": action})).unwrap();
    let out = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&PDF).unwrap().outputs.first().unwrap().name.clone();
    let edge = nodecraft_graph::Edge {
        id: nodecraft_types::EdgeId::from_bytes([72; 16]),
        from: nodecraft_graph::Endpoint { node: PDF, port: out },
        to: nodecraft_graph::Endpoint { node: PAGES, port: "document".into() },
        order: 0,
    };
    h.state_mut().session.dispatch("edge.connect", &serde_json::json!({"canvas": canvas, "edge": edge})).unwrap();
    h.run();
    h.state_mut().open_reader(PAGES);
    h.run();
    assert_eq!(h.state().reader.as_ref().map(|reader| (reader.node, reader.target)), Some((PDF, Some(PAGES))));
    h.key_press(egui::Key::ArrowRight);
    h.run();
    h.get_by_label("Only this page").click();
    h.run();
    let range = |h: &Harness<'static, NodeCraftApp>| {
        let node = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&PAGES).unwrap().clone();
        (node.params.get("first").cloned(), node.params.get("last").cloned())
    };
    assert_eq!(range(&h), (Some(Value::Unsigned(Unsigned(2))), Some(Value::Unsigned(Unsigned(2)))), "{:?}", h.state().notice);
    h.key_press(egui::Key::End);
    h.run();
    h.get_by_label("End here").click();
    h.run();
    assert_eq!(range(&h), (Some(Value::Unsigned(Unsigned(2))), Some(Value::Unsigned(Unsigned(3)))));
    h.state_mut().session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(range(&h), (Some(Value::Unsigned(Unsigned(2))), Some(Value::Unsigned(Unsigned(2)))));
}

#[test]
#[ignore = "manual public-safe capture; READER590_RENDER_DIR"]
fn reader590_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("READER590_RENDER_DIR").unwrap());
    let mut h = with_pdf();
    h.state_mut().open_reader(PDF);
    h.key_press(egui::Key::ArrowRight);
    h.run();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "reader590-dark.png"), (crate::theme::ThemeKind::Light, "reader590-light.png")] {
        h.state_mut().theme = theme;
        // Textures belong to the context that made them: decode again in the drawing one.
        h.state_mut().previews = Default::default();
        let mut shot =
            Harness::builder().wgpu().with_size(egui::Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        // Page previews decode on a worker.
        for _ in 0..40 {
            shot.run();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(egui::Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
}
