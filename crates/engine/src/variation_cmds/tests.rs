use crate::Session;
use nodecraft_exec::variations::VARIATIONS_CONFIG;
use nodecraft_types::{CanvasId, NodeId, Unsigned, Value as Wire};
use serde_json::json;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMAGE: NodeId = NodeId::from_bytes([3; 16]);
const TEXT: NodeId = NodeId::from_bytes([4; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": IMAGE, "kind": "generation.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": TEXT, "kind": "source.text", "pos": {"x": 400, "y": 0}})).unwrap();
    s
}

fn stored(s: &Session) -> Option<Wire> {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&IMAGE).unwrap().config.get(VARIATIONS_CONFIG).cloned()
}

#[test]
fn the_count_is_set_undone_and_reset_as_one_edit_each() {
    let mut s = session();
    assert_eq!(s.dispatch("node.variations_set", &json!({"canvas": CANVAS, "id": IMAGE, "count": 4})).unwrap(), json!({"count": 4}));
    assert_eq!(stored(&s), Some(Wire::Unsigned(Unsigned(4))));
    let canvas = s.document().canvases().get(&CANVAS).unwrap().clone();
    assert!(nodecraft_exec::staleness::stale(&canvas, s.kinds()).unwrap().is_empty(), "the count never makes a result stale");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(stored(&s), None);
    s.dispatch("edit.redo", &json!({})).unwrap();
    // One is the default, so it is stored as nothing, like null.
    let revision = s.revision();
    assert_eq!(s.dispatch("node.variations_set", &json!({"canvas": CANVAS, "id": IMAGE, "count": 1})).unwrap(), json!({"count": 1}));
    assert_eq!(stored(&s), None);
    assert_ne!(s.revision(), revision);
    let revision = s.revision();
    assert_eq!(s.dispatch("node.variations_set", &json!({"canvas": CANVAS, "id": IMAGE, "count": null})).unwrap(), json!({"count": 1}));
    assert_eq!(s.revision(), revision, "no edit when nothing changes");
}

#[test]
fn bad_requests_are_errors_that_change_nothing() {
    let mut s = session();
    let revision = s.revision();
    for params in [
        json!({"canvas": CANVAS, "id": IMAGE, "count": 0}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": 21}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": u64::MAX}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": -1}),
        json!({"canvas": CANVAS, "id": IMAGE, "count": "3"}),
        json!({"canvas": CANVAS, "id": TEXT, "count": 3}),
        json!({"canvas": CANVAS, "id": NodeId::from_bytes([9; 16]), "count": 3}),
        json!({"canvas": CanvasId::from_bytes([9; 16]), "id": IMAGE, "count": 3}),
    ] {
        assert!(s.dispatch("node.variations_set", &params).is_err(), "{params}");
    }
    assert_eq!(s.revision(), revision);
}

fn canvas(s: &Session) -> &nodecraft_graph::Canvas {
    s.document().canvases().get(&CANVAS).unwrap()
}

fn result(n: u8) -> serde_json::Value {
    json!({"id": nodecraft_types::GenerationId::from_bytes([n; 16]), "outputs": [{"type": "text", "value": format!("take {n}")}]})
}

#[test]
fn the_canvas_preference_is_one_edit_undone_like_any_other() {
    let mut s = session();
    assert!(!canvas(&s).variation_cards(), "history is the default");
    s.dispatch("canvas.set_variation_cards", &json!({"canvas": CANVAS, "cards": true})).unwrap();
    assert!(canvas(&s).variation_cards());
    let revision = s.revision();
    s.dispatch("canvas.set_variation_cards", &json!({"canvas": CANVAS, "cards": true})).unwrap();
    assert_eq!(s.revision(), revision, "no edit when nothing changes");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!canvas(&s).variation_cards());
    assert!(s.dispatch("canvas.set_variation_cards", &json!({"canvas": CanvasId::from_bytes([9; 16]), "cards": true})).is_err());
    assert!(s.dispatch("canvas.set_variation_cards", &json!({"canvas": CANVAS, "cards": "yes"})).is_err());
}

#[test]
fn a_variation_recorded_as_a_card_copies_the_source_and_its_inputs_beside_it_in_one_step() {
    let mut s = session();
    let edge = nodecraft_types::EdgeId::from_bytes([5; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": TEXT, "port": "out"}, "to": {"node": IMAGE, "port": "prompt"}, "order": 0}}),
    )
    .unwrap();
    // A card where the first spot is taken goes further down.
    let blocker = NodeId::from_bytes([6; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": blocker, "kind": "source.text", "pos": {"x": 360, "y": 0}})).unwrap();
    let card = super::card_id(nodecraft_types::GenerationId::from_bytes([10; 16]));
    let before = canvas(&s).clone();
    s.dispatch("run.record", &json!({"canvas": CANVAS, "results": [{"node": IMAGE, "generation": result(10), "card": card}]})).unwrap();
    let c = canvas(&s);
    let (source, copy) = (c.nodes().get(&IMAGE).unwrap(), c.nodes().get(&card).unwrap());
    assert!(source.history.is_empty(), "the variation is on the card, not in the source's history");
    assert_eq!(copy.history.len(), 1);
    assert_eq!(copy.active, Some(nodecraft_types::GenerationId::from_bytes([10; 16])));
    assert_eq!((copy.kind.as_str(), &copy.params), (source.kind.as_str(), &source.params));
    assert_eq!(copy.config.get(VARIATIONS_CONFIG), None, "a card is one variation");
    assert!(copy.pos.x > source.pos.x && copy.pos.y > 0.0, "beside the source, past the taken spot: {:?}", copy.pos);
    assert!(c.edges().values().any(|e| e.from.node == TEXT && e.to.node == card), "the card is fed what the source is");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(canvas(&s).nodes().get(&card).is_none(), "one undo removes the card and its record");
    assert_eq!(canvas(&s).edges().len(), before.edges().len());
    // A card id that is taken is refused, with nothing recorded.
    let revision = s.revision();
    assert!(s.dispatch("run.record", &json!({"canvas": CANVAS, "results": [{"node": IMAGE, "generation": result(12), "card": TEXT}]})).is_err());
    assert!(s.dispatch("run.record", &json!({"canvas": CANVAS, "results": [{"node": IMAGE, "generation": result(12), "card": card, "keep": true}]})).is_err());
    assert_eq!(s.revision(), revision);
}

#[test]
fn a_variation_recorded_late_goes_before_the_later_ones_and_keeps_the_active_result() {
    let mut s = session();
    let (first, last) = (nodecraft_types::GenerationId::from_bytes([20; 16]), nodecraft_types::GenerationId::from_bytes([21; 16]));
    s.dispatch("run.record", &json!({"canvas": CANVAS, "results": [{"node": IMAGE, "generation": result(21)}]})).unwrap();
    s.dispatch("run.record", &json!({"canvas": CANVAS, "results": [{"node": IMAGE, "generation": result(20), "later": [last]}]})).unwrap();
    let node = canvas(&s).nodes().get(&IMAGE).unwrap();
    assert_eq!(node.history.iter().map(|g| g.id).collect::<Vec<_>>(), vec![first, last], "in variant order");
    assert_eq!(node.active, Some(last), "the last variation stays active");
}

#[test]
fn cards_recorded_together_take_separate_spots() {
    let mut s = session();
    // A second image card just under the first: their cards would land on one spot.
    let below = NodeId::from_bytes([7; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": below, "kind": "generation.image", "pos": {"x": 0, "y": 100}})).unwrap();
    let (a, b) = (super::card_id(nodecraft_types::GenerationId::from_bytes([30; 16])), super::card_id(nodecraft_types::GenerationId::from_bytes([31; 16])));
    let results = json!([{"node": IMAGE, "generation": result(30), "card": a}, {"node": below, "generation": result(31), "card": b}]);
    s.dispatch("run.record", &json!({"canvas": CANVAS, "results": results})).unwrap();
    let c = canvas(&s);
    let (a, b) = (c.nodes().get(&a).unwrap().pos, c.nodes().get(&b).unwrap().pos);
    assert!((a.x - b.x).abs() >= 360.0 || (a.y - b.y).abs() >= 600.0, "{a:?} {b:?}");
}
