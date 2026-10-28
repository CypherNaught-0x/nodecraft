//! The batch items review (#92): step through a fanned result's items, and see a Cross of two
//! batches as a grid, failed items included, driven by real pointer events.
use super::batch_review::{ItemState, review};
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_graph::{Generation, Incomplete, ItemKey, Missing, MissingItem};
use nodecraft_types::GenerationId;

const SUBJECTS: NodeId = NodeId::from_bytes([1; 16]);
const TIMES: NodeId = NodeId::from_bytes([2; 16]);
const CARD: NodeId = NodeId::from_bytes([3; 16]);

fn batch(session: &Session, id: NodeId, title: &str, text: &str, x: f32) -> Node {
    let mut node = session.kinds().get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(id, Default::default(), &Default::default()).unwrap();
    node.title = title.into();
    node.params.insert("text".into(), Value::Text(text.into()));
    node.pos = nodecraft_graph::Position { x, y: -400.0 };
    node
}

/// Subjects (a fox, an owl) × times (at dawn, at night) crossed into a text card whose last
/// combination failed; `pairs` lists which combinations the result holds, in order.
fn project(pairs: &[(usize, usize)], failed: Option<usize>) -> (Session, CanvasId) {
    let canvas = CanvasId::from_bytes([9; 16]);
    let scratch = Session::new(Project::new(ProjectId::from_bytes([8; 16]), "Scratch").unwrap()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([7; 16]), "Review").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    for node in [batch(&scratch, SUBJECTS, "Subjects", "a fox\nan owl", -300.0), batch(&scratch, TIMES, "Times", "at dawn\nat night", 300.0)] {
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    let listed = |id: NodeId| {
        let canvas = project.canvases().get(&canvas).unwrap();
        nodecraft_exec::planner::batch_items(canvas, scratch.kinds(), id).unwrap()
    };
    let (subjects, times) = (listed(SUBJECTS), listed(TIMES));
    let mut node = Node::new(CARD, "generation.text");
    node.pos = nodecraft_graph::Position { x: 0.0, y: 0.0 };
    node.outputs = vec![nodecraft_graph::Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }];
    let made = GenerationId::from_bytes([5; 16]);
    let words = |value: &Value| if let Value::Text(text) = value { text.split(' ').next_back().unwrap_or("").to_owned() } else { String::new() };
    let values = pairs
        .iter()
        .enumerate()
        .map(
            |(index, (s, t))| {
                if Some(index) == failed { Value::List(Vec::new()) } else { Value::Text(format!("{} {}", words(&subjects[*s].1), words(&times[*t].1))) }
            },
        )
        .collect();
    let mut result = Generation::new(made, vec![Value::List(values)]);
    result.items = pairs.iter().map(|(s, t)| vec![ItemKey { batch: SUBJECTS, item: subjects[*s].0 }, ItemKey { batch: TIMES, item: times[*t].0 }]).collect();
    result.incomplete =
        failed.map(|item| Incomplete { missing: vec![MissingItem { item: u32::try_from(item).unwrap(), why: Missing::Failed, keys: Vec::new() }] });
    node.history.push(result);
    node.active = Some(made);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    (Session::new(project).unwrap(), canvas)
}

const CROSS: [(usize, usize); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];

fn press_at(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, button: egui::PointerButton) {
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
}

#[test]
fn a_crossed_result_reviews_as_a_grid_with_its_failed_cell() {
    let (session, canvas) = project(&CROSS, Some(3));
    let node = session.document().canvases().get(&canvas).unwrap().nodes().get(&CARD).unwrap().clone();
    let review = review(session.document().canvases().get(&canvas).unwrap(), session.kinds(), &node).unwrap();
    assert_eq!(review.axes.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Subjects", "Times"]);
    assert_eq!(review.axes[0].labels, ["a fox", "an owl"]);
    assert_eq!(review.axes[1].labels, ["at dawn", "at night"]);
    assert_eq!(review.grid, Some(vec![vec![0, 1], vec![2, 3]]));
    assert_eq!(review.items[1], (vec!["a fox".to_owned(), "at night".to_owned()], ItemState::Done(Value::Text("fox night".into()))));
    assert_eq!(review.items[3].1, ItemState::Failed);
}

#[test]
fn a_zip_or_a_changed_batch_has_no_grid_but_every_item_is_listed() {
    // Zipped: item by item, so no grid of combinations.
    let (session, canvas) = project(&[(0, 0), (1, 1)], None);
    let c = session.document().canvases().get(&canvas).unwrap();
    let r = review(c, session.kinds(), c.nodes().get(&CARD).unwrap()).unwrap();
    assert_eq!(r.grid, None);
    assert_eq!(r.items.len(), 2);

    // An item the batch no longer holds is still listed, and says so.
    let (mut session, canvas) = project(&CROSS, None);
    session.dispatch("node.param_set", &json!({"canvas": canvas, "id": SUBJECTS, "name": "text", "value": {"type": "text", "value": "a fox\na bee"}})).unwrap();
    let c = session.document().canvases().get(&canvas).unwrap();
    let r = review(c, session.kinds(), c.nodes().get(&CARD).unwrap()).unwrap();
    assert_eq!(r.axes[0].labels, ["a fox", "Item no longer in the batch (1)"]);
    assert_eq!(r.grid.as_ref().map(Vec::len), Some(2), "still every combination of what it was made from");

    // A result not made per item has nothing to review.
    let mut plain = c.nodes().get(&CARD).unwrap().clone();
    plain.history.iter_mut().for_each(|g| g.items.clear());
    assert!(!super::batch_review::reviewable(&plain));
    assert!(review(c, session.kinds(), &plain).is_none());
}

#[test]
fn review_items_steps_through_each_item_and_the_grid_picks_one() {
    let (session, canvas) = project(&CROSS, Some(3));
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1400.0, 900.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    // The card's own context menu opens the review.
    let point = h.state().view.rect_to_screen(h.state().viewport.min, h.state().painted[&CARD]).left_center() + Vec2::new(4.0, 0.0);
    press_at(&mut h, point, egui::PointerButton::Secondary);
    h.get_by_label("Review items…").click();
    h.run();
    assert!(h.query_by_label("Item 1 of 4").is_some());
    assert!(h.query_by_label("Subjects: a fox").is_some() && h.query_by_label("Times: at dawn").is_some());

    h.get_by_label("Next ▶").click();
    h.run();
    assert!(h.query_by_label("Item 2 of 4").is_some());
    assert!(h.query_by_label("Times: at night").is_some());

    // The grid has every combination, the failed one marked; picking it shows why.
    for cell in ["fox dawn", "fox night", "owl dawn"] {
        assert!(h.query_all_by_label(cell).next().is_some(), "{cell}");
    }
    h.get_by_label("Failed").click();
    h.run();
    assert!(h.query_by_label("Item 4 of 4").is_some());
    assert!(h.query_by_label_contains("Run the card again").is_some());
    assert!(h.query_by_label("Next ▶").is_some_and(|next| next.accesskit_node().is_disabled()), "the last item");

    h.get_by_label("◀ Previous").click();
    h.run();
    assert!(h.query_by_label("Item 3 of 4").is_some());
    assert_eq!(h.state().batch_review.as_ref().map(|r| r.item), Some(2));
    // Nothing in the document changed.
    assert!(!h.state().session.can_undo());
}
