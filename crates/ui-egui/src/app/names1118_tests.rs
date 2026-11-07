//! #1118: every clickable control has a spoken name, and a tag marker names its tag rather than
//! showing it by colour alone.
use super::*;
use egui_kittest::kittest::Queryable;

#[test]
fn a_tagged_card_names_its_tag_and_no_control_is_only_a_glyph() {
    let mut h = super::tests::harness();
    h.run();
    let canvas = h.state().canvas;
    let node = super::tests::node(h.state()).id;
    let tag = nodecraft_types::TagId::from_bytes([0x18; 16]);
    let session = &mut h.state_mut().session;
    session.dispatch("tag.put", &json!({"canvas": canvas, "tag": {"id": tag, "title": "Hero shots", "color": "pink"}})).unwrap();
    session.dispatch("node.tag_set", &json!({"canvas": canvas, "nodes": [node], "tag": tag})).unwrap();
    h.run();
    assert!(h.query_by_label("Tag: Hero shots").is_some(), "the marker is named, not only coloured");
    assert_eq!(crate::a11y_check::unnamed_controls(&h), Vec::<String>::new());
}
