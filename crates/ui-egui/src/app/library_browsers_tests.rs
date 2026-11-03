//! The Library's Assets, History and Styles tabs (#96), driven through real pointer events.
use super::tests::harness;
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_types::{AssetRef, GenerationId};

const PHOTO: NodeId = NodeId::from_bytes([60; 16]);
const TEXT: NodeId = NodeId::from_bytes([3; 16]);
const INK: NodeId = NodeId::from_bytes([70; 16]);
const NEON: NodeId = NodeId::from_bytes([71; 16]);
const SOUL: NodeId = NodeId::from_bytes([72; 16]);

fn canvas<'a>(h: &'a Harness<'static, NodeCraftApp>) -> &'a nodecraft_graph::Canvas {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap()
}

/// Opens the Library on `tab`.
fn open(h: &mut Harness<'static, NodeCraftApp>, tab: &str) {
    crate::app::topbar_tests::reveal(h, "Techniques");
    h.get_by_label("Techniques").click();
    h.run();
    h.get_by_label(tab).click();
    h.run();
    h.run();
}

/// Frames until a widget labelled `label` shows (thumbnails decode off the frame).
fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str) -> bool {
    for _ in 0..200 {
        h.run();
        if h.query_by_label(label).is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// An image card titled "Holiday photo", far from the view's middle, holding a small PNG.
fn with_photo(h: &mut Harness<'static, NodeCraftApp>) -> AssetRef {
    let png = nodecraft_media::encode_png(6, 4, &[120; 6 * 4 * 4], &Default::default()).unwrap();
    let on = h.state().canvas;
    let session = &mut h.state_mut().session;
    let hash = session.import_asset(&png).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    session.dispatch("node.create", &json!({"canvas": on, "id": PHOTO, "kind": "source.image", "pos": {"x": 2000, "y": 2000}})).unwrap();
    session.dispatch("node.title", &json!({"canvas": on, "id": PHOTO, "title": "Holiday photo"})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": on, "id": PHOTO, "name": "asset", "value": Value::Media(asset.clone())})).unwrap();
    h.run();
    asset
}

/// The image cards on the open canvas holding `asset`.
fn holding(h: &Harness<'static, NodeCraftApp>, asset: &AssetRef) -> Vec<NodeId> {
    canvas(h).nodes().values().filter(|n| matches!(n.params.get("asset"), Some(Value::Media(a)) if a.hash == asset.hash)).map(|n| n.id).collect()
}

#[test]
fn the_assets_tab_shows_a_thumbnail_filters_by_kind_and_inserts_the_asset_in_one_step() {
    let mut h = harness();
    let asset = with_photo(&mut h);
    open(&mut h, "Assets");
    assert!(wait_for(&mut h, "Thumbnail of Holiday photo"), "the image's thumbnail shows");
    assert!(h.query_by_label("Image · Canvas").is_some() || h.query_by_label_contains("Image · ").is_some());

    h.get_by_label("Sounds").click();
    h.run();
    h.run();
    assert!(h.query_by_label("No asset matches.").is_some(), "only sounds now");
    h.get_by_label("All").click();
    h.run();
    h.run();
    assert!(wait_for(&mut h, "Thumbnail of Holiday photo"));

    let before = h.state().session.document().clone();
    h.get_by_label("Insert").click();
    h.run();
    let cards = holding(&h, &asset);
    assert_eq!(cards.len(), 2, "a second card holds the same asset: {:?}", h.state().notice);
    let inserted = cards.into_iter().find(|id| *id != PHOTO).unwrap();
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), [inserted]);
    let node = canvas(&h).nodes().get(&inserted).unwrap();
    assert_eq!(node.kind, "source.image");
    let expected = h.state().view.to_world(h.state().viewport.min, h.state().navigation.usable(h.state().viewport, h.state().theme.tokens()).center());
    let rect = payload_rect(&h.state().session, node, h.state().theme.tokens());
    assert!((rect.center() - expected).length() < 1.0, "centred in the view: {:?} vs {expected:?}", rect.center());
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "inserting is one undo step");
}

fn g(n: u8) -> GenerationId {
    GenerationId::from_bytes([0x40 + n; 16])
}

#[test]
fn the_history_tab_restores_an_older_result_and_places_one_as_a_card() {
    let mut h = harness();
    let on = h.state().canvas;
    for i in 1..=2 {
        let results = json!([{"node": TEXT, "generation": {"id": g(i), "outputs": [{"type": "text", "value": format!("Take {i}: a forest")}]}}]);
        h.state_mut().session.dispatch("run.record", &json!({"canvas": on, "results": results})).unwrap();
    }
    h.run();
    let label = {
        let node = canvas(&h).nodes().get(&TEXT).unwrap();
        if node.title.trim().is_empty() { node.kind.clone() } else { node.title.clone() }
    };
    open(&mut h, "History");
    assert!(h.query_by_label(&format!("{label} · 2/2")).is_some(), "newest first");
    assert!(h.query_by_label(&format!("{label} · 1/2")).is_some());
    assert!(h.query_by_label("Take 1: a forest").is_some(), "a text result is quoted");

    // The newest is in use; the older one's "Use this result" makes it active.
    let uses: Vec<_> = h.query_all_by_label("Use this result").collect();
    assert_eq!(uses.len(), 2);
    uses[1].click();
    h.run();
    assert_eq!(canvas(&h).nodes().get(&TEXT).unwrap().active, Some(g(1)), "{:?}", h.state().notice);

    let before = canvas(&h).nodes().len();
    h.query_all_by_label("Place").next().unwrap().click();
    h.run();
    assert_eq!(canvas(&h).nodes().len(), before + 1, "{:?}", h.state().notice);
    let placed = *h.state().selection.nodes().iter().next().unwrap();
    let node = canvas(&h).nodes().get(&placed).unwrap();
    assert_eq!(node.params.get("text"), Some(&Value::Text("Take 2: a forest".into())), "the newest result, as a source card");

    // Only the selected card's results, when asked.
    h.state_mut().selection.only(placed);
    h.run();
    h.get_by_label("Selected card only").click();
    h.run();
    h.run();
    assert!(h.query_by_label_contains("No results on this canvas yet").is_some(), "the placed card has none");
}

/// A Train style card holding a trained style of `family` kept by `provider`.
fn add_style(h: &mut Harness<'static, NodeCraftApp>, id: NodeId, title: &str, provider: &str, family: &str, trigger: &str) {
    let on = h.state().canvas;
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": on, "id": id, "kind": nodecraft_nodes::generation::STYLE_KIND, "pos": {"x": 900, "y": 900}})).unwrap();
    session.dispatch("node.title", &json!({"canvas": on, "id": id, "title": title})).unwrap();
    let style = nodecraft_media::style::Style {
        provider: provider.into(),
        trainer: "trainer".into(),
        family: family.into(),
        trigger: trigger.into(),
        weights: format!("https://example.com/files/{trigger}"),
    };
    let hash = session.import_asset(&nodecraft_media::style::encode(&style).unwrap()).unwrap();
    let asset = AssetRef { hash, mime: format!("application/vnd.nodecraft.style+json; family={family}").parse().unwrap(), meta: Default::default() };
    let generation = GenerationId::from_bytes([id.as_bytes()[0].wrapping_add(100); 16]);
    let results = json!([{"node": id, "generation": {"id": generation, "outputs": [Value::Media(asset)]}}]);
    session.dispatch("run.record", &json!({"canvas": on, "results": results})).unwrap();
    h.run();
}

#[test]
fn the_styles_tab_marks_soul_ids_and_the_provider_and_uses_a_style_on_the_selected_card() {
    let mut h = harness();
    add_style(&mut h, INK, "Ink wash", "fal", "flux-1", "inkwash");
    add_style(&mut h, NEON, "Neon", "fal", "flux-1", "neonish");
    add_style(&mut h, SOUL, "Mara", "higgsfield", "soul-2", "mara");
    // A FLUX card wired from the ink style.
    let on = h.state().canvas;
    let card = NodeId::from_bytes([80; 16]);
    let edge = nodecraft_types::EdgeId::from_bytes([81; 16]);
    h.state_mut().session.dispatch("style.card", &json!({"canvas": on, "style": INK, "id": card, "edge": edge, "pos": {"x": 1200, "y": 900}})).unwrap();
    h.run();
    open(&mut h, "Styles");
    assert!(h.query_by_label("Soul ID").is_some(), "the Soul ID is marked");
    assert_eq!(h.query_all_by_label("Kept by fal: only that account can use it").count(), 2);
    assert!(h.query_by_label("Kept by higgsfield: only that account can use it").is_some());
    let uses: Vec<_> = h.query_all_by_label("Use on selected card").collect();
    assert_eq!(uses.len(), 3);
    assert!(uses.iter().all(|button| button.accesskit_node().is_disabled()), "nothing is selected");

    h.state_mut().selection.only(card);
    h.run();
    h.run();
    let uses: Vec<_> = h.query_all_by_label("Use on selected card").collect();
    assert!(!uses[1].accesskit_node().is_disabled(), "the card takes FLUX styles");
    assert!(uses[2].accesskit_node().is_disabled(), "SOUL 2 identities aren't wired into a FLUX card");
    let before = h.state().session.document().clone();
    uses[1].click();
    h.run();
    let wired: Vec<NodeId> = canvas(&h).edges().values().filter(|e| e.to.node == card && e.to.port == "style").map(|e| e.from.node).collect();
    assert_eq!(wired, [NEON], "the neon style replaced the ink one: {:?}", h.state().notice);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "one undo step");

    // A trained style is a result too, but it can't become a source card: Place is disabled.
    h.get_by_label("History").click();
    h.run();
    h.run();
    let places: Vec<_> = h.query_all_by_label("Place").collect();
    assert_eq!(places.len(), 3, "each style card's result is listed");
    assert!(places.iter().all(|button| button.accesskit_node().is_disabled()));
}

const PAINTER: NodeId = NodeId::from_bytes([90; 16]);

/// An image card titled "Painter", far from the view's middle, with two image results.
fn with_paintings(h: &mut Harness<'static, NodeCraftApp>) {
    let on = h.state().canvas;
    let session = &mut h.state_mut().session;
    let mut node = session.kinds().get("generation.image").unwrap().descriptor().instantiate(PAINTER, &BTreeMap::new()).unwrap();
    node.pos = nodecraft_graph::Position { x: 3000.0, y: 3000.0 };
    session.dispatch("node.add", &json!({"canvas": on, "node": node})).unwrap();
    session.dispatch("node.title", &json!({"canvas": on, "id": PAINTER, "title": "Painter"})).unwrap();
    for i in 1..=2u8 {
        let png = nodecraft_media::encode_png(3, 3, &[40 * i; 36], &Default::default()).unwrap();
        let hash = session.import_asset(&png).unwrap();
        let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
        let results = json!([{"node": PAINTER, "generation": {"id": g(10 + i), "outputs": [Value::Media(asset)]}}]);
        session.dispatch("run.record", &json!({"canvas": on, "results": results})).unwrap();
    }
    h.run();
}

#[test]
fn ticked_results_go_on_the_canvas_together_in_one_undo_step() {
    let mut h = harness();
    with_paintings(&mut h);
    open(&mut h, "History");
    assert!(h.query_by_label("Place on canvas").is_none(), "no bar until something is ticked");
    h.get_by_label("Painter · 2/2").click();
    h.run();
    h.get_by_label("Painter · 1/2").click();
    h.run();
    assert!(h.query_by_label("2 ticked").is_some());
    let before = h.state().session.document().clone();
    let count = canvas(&h).nodes().len();
    h.get_by_label("Place on canvas").click();
    h.run();
    assert_eq!(canvas(&h).nodes().len(), count + 2, "{:?}", h.state().notice);
    let placed: Vec<NodeId> = h.state().selection.nodes().iter().copied().collect();
    assert_eq!(placed.len(), 2, "the placed cards are selected");
    let mut titles: Vec<String> = placed.iter().map(|id| canvas(&h).nodes()[id].title.clone()).collect();
    titles.sort();
    assert_eq!(titles, ["Painter · 1/2", "Painter · 2/2"], "each card says where it came from");
    assert!(placed.iter().all(|id| canvas(&h).nodes()[id].kind == "source.image"));
    assert!(h.query_by_label("2 ticked").is_none(), "placing clears the ticks");
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "one undo step for all of them");
}

#[test]
fn ticked_assets_are_placed_and_only_results_on_this_canvas_export() {
    let mut h = harness();
    let photo = with_photo(&mut h);
    with_paintings(&mut h);
    open(&mut h, "Assets");
    assert!(wait_for(&mut h, "Thumbnail of Holiday photo"));
    h.get_by_label("Holiday photo").click();
    h.run();
    assert!(h.get_by_label("Export…").accesskit_node().is_disabled(), "a card's own asset is not a result to export");
    let painted: Vec<_> = h.query_all_by_label("Painter").collect();
    assert_eq!(painted.len(), 2);
    painted[0].click();
    h.run();
    assert!(h.query_by_label("2 ticked").is_some());
    let count = canvas(&h).nodes().len();
    h.get_by_label("Place on canvas").click();
    h.run();
    assert_eq!(canvas(&h).nodes().len(), count + 2, "{:?}", h.state().notice);
    assert_eq!(holding(&h, &photo).len(), 2, "the photo again, from the project's stored bytes");
    // Ticking a painting alone allows Export: an entry labelled with its card was found in a result.
    h.query_all_by_label("Painter").next().unwrap().click();
    h.run();
    assert!(!h.get_by_label("Export…").accesskit_node().is_disabled());
    h.get_by_label("Clear ticks").click();
    h.run();
    assert!(h.query_by_label("Export…").is_none());
}

#[test]
#[cfg(unix)]
fn ticked_results_export_through_the_workflow_result_save_after_a_folder_is_chosen() {
    let _serial = super::export_tests::EXPORT_TESTS.lock().unwrap();
    let mut h = harness();
    with_paintings(&mut h);
    open(&mut h, "History");
    h.get_by_label("Painter · 2/2").click();
    h.run();
    h.get_by_label("Painter · 1/2").click();
    h.run();
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().exports.answer = Some(dir.path().to_path_buf());
    h.get_by_label("Export…").click();
    h.run_steps(3);
    assert!(h.query_by_label("Save workflow results").is_some(), "{:?}", h.state().notice);
    assert!(h.query_by_label("Only the chosen historical result items.").is_some());
    assert!(h.query_by_label("2 media files; 2 ready").is_some());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is written before Save files");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
}

#[test]
fn a_ticked_result_that_is_deleted_is_unticked() {
    let mut h = harness();
    with_paintings(&mut h);
    open(&mut h, "History");
    h.get_by_label("Painter · 2/2").click();
    h.run();
    h.get_by_label("Painter · 1/2").click();
    h.run();
    assert!(h.query_by_label("2 ticked").is_some());
    let on = h.state().canvas;
    h.state_mut().session.dispatch("history.delete", &json!({"canvas": on, "id": PAINTER, "generation": g(11)})).unwrap();
    h.run();
    h.run();
    assert!(h.query_by_label("1 ticked").is_some(), "the deleted result's tick is gone");
    let count = canvas(&h).nodes().len();
    h.get_by_label("Place on canvas").click();
    h.run();
    assert_eq!(canvas(&h).nodes().len(), count + 1, "the rest still places: {:?}", h.state().notice);
    // Undoing the placement and the deletion brings the result back, unticked.
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert!(h.query_by_label("Place on canvas").is_none());
}

/// My library: a ticked asset is saved, listed under the My library scope with its project's
/// title, placed back as a card in one undo step, and removed.
#[test]
fn a_ticked_asset_is_saved_to_my_library_placed_from_it_and_removed() {
    let mut h = harness();
    let asset = with_photo(&mut h);
    open(&mut h, "Assets");
    h.get_by_label("Holiday photo").click();
    h.run();
    h.get_by_label("Save to My library").click();
    h.run();
    assert_eq!(h.state().notice.as_deref(), Some("1 saved to My library"));
    assert!(h.query_by_label("Save to My library").is_none(), "the ticks were cleared");

    h.get_by_label("My library").click();
    h.run();
    h.run();
    assert!(h.query_by_label("Image Holiday photo").is_some(), "the entry is listed: {:?}", h.state().notice);
    assert!(h.query_by_label_contains("Image · from ").is_some(), "with the project it came from");
    assert!(h.query_by_label_contains("1 file · ").is_some());

    let before = h.state().session.document().clone();
    h.get_by_label("Place").click();
    h.run();
    let cards = holding(&h, &asset);
    assert_eq!(cards.len(), 2, "{:?}", h.state().notice);
    let placed = cards.into_iter().find(|id| *id != PHOTO).unwrap();
    assert_eq!(canvas(&h).nodes()[&placed].title, "Holiday photo");
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), [placed]);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before, "placing is one undo step");

    h.get_by_label("Remove from My library").click();
    h.run();
    h.run();
    assert_eq!(h.state().notice.as_deref(), Some("Removed 1 file from My library."));
    assert!(h.query_by_label_contains("Nothing saved yet").is_some());
    h.get_by_label("This project").click();
    h.run();
    h.run();
    assert!(h.query_by_label("Holiday photo").is_some(), "the project's own asset stays");
}
