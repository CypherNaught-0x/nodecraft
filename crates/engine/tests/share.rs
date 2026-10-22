//! Roles a share link gives a session (#121): what each may do to the document and its
//! discussions, and the share.link_* commands.
mod common;
use std::cell::RefCell;
use std::rc::Rc;

use nodecraft_engine::Session;
use nodecraft_engine::access::Access;
use nodecraft_engine::collab::{Collab, LINKS_SERVICE};
use nodecraft_sync::{Role, RoomId};
use nodecraft_types::{CommentId, ErrorKind};
use serde_json::{Value, json};

fn id(n: u8) -> CommentId {
    CommentId::from_bytes([n; 16])
}
fn note(n: u8, text: &str) -> Value {
    json!({"canvas": common::canvas(), "id": id(n), "text": text, "pos": {"x": 100, "y": 200}})
}
fn session(role: Role, name: &str) -> Session {
    let mut s = Session::new(common::project()).unwrap();
    s.set_access(Access::new(role, Some(name.into())).unwrap());
    s
}
fn thread(s: &Session, n: u8) -> nodecraft_graph::Comment {
    s.document().canvases()[&common::canvas()].comments()[&id(n)].clone()
}
fn forbidden(s: &mut Session, command: &str, params: Value) {
    assert_eq!(s.dispatch(command, &params).unwrap_err().kind, ErrorKind::Forbidden, "{command}");
}

#[test]
fn a_viewer_changes_nothing() {
    let mut s = session(Role::View, "Ana");
    let before = s.document().clone();
    forbidden(&mut s, "node.add", json!({"canvas": common::canvas(), "node": common::node(5)}));
    forbidden(&mut s, "node.remove", json!({"canvas": common::canvas(), "id": common::node_id(1)}));
    forbidden(&mut s, "comment.create", note(10, "Hmm"));
    assert_eq!(s.document(), &before);
}

#[test]
fn a_commenter_writes_discussions_in_their_name_and_nothing_else() {
    let mut s = session(Role::Comment, "Ana");
    forbidden(&mut s, "node.add", json!({"canvas": common::canvas(), "node": common::node(5)}));
    s.dispatch("comment.create", &note(10, "Lovely")).unwrap();
    assert_eq!(thread(&s, 10).author.as_deref(), Some("Ana"));
    s.dispatch("comment.reply", &json!({"canvas": common::canvas(), "id": id(10), "reply": id(11), "text": "Me again"})).unwrap();
    assert_eq!(thread(&s, 10).replies[0].author.as_deref(), Some("Ana"));
    // Undo takes back a discussion change, which a commenter may make.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(thread(&s, 10).replies.is_empty());

    // Another person's thread: answer and resolve it, but not reword, move or delete it.
    let mut other = thread(&s, 10);
    other.id = id(20);
    other.author = Some("Bo".into());
    s.apply_remote(nodecraft_graph::Changeset::new(vec![nodecraft_graph::Change::PutComment { canvas: common::canvas(), comment: other }])).unwrap();
    forbidden(&mut s, "comment.edit", json!({"canvas": common::canvas(), "id": id(20), "text": "Mine"}));
    forbidden(&mut s, "comment.move", json!({"canvas": common::canvas(), "id": id(20), "pos": {"x": 0, "y": 0}}));
    forbidden(&mut s, "comment.remove", json!({"canvas": common::canvas(), "id": id(20)}));
    s.dispatch("comment.resolve", &json!({"canvas": common::canvas(), "id": id(20), "resolved": true})).unwrap();
    s.dispatch("comment.reply", &json!({"canvas": common::canvas(), "id": id(20), "reply": id(21), "text": "Agreed"})).unwrap_err();
    s.dispatch("comment.resolve", &json!({"canvas": common::canvas(), "id": id(20), "resolved": false})).unwrap();
    s.dispatch("comment.reply", &json!({"canvas": common::canvas(), "id": id(20), "reply": id(21), "text": "Agreed"})).unwrap();
    s.dispatch("comment.remove", &json!({"canvas": common::canvas(), "id": id(10)})).unwrap();
}

#[test]
fn an_editor_tidies_anyones_discussion_but_rewords_only_their_own() {
    let mut s = session(Role::Edit, "Cy");
    let mut other = nodecraft_graph::Comment::new(id(20), "Bo's", nodecraft_graph::Position { x: 0.0, y: 0.0 });
    other.author = Some("Bo".into());
    s.apply_remote(nodecraft_graph::Changeset::new(vec![nodecraft_graph::Change::PutComment { canvas: common::canvas(), comment: other }])).unwrap();
    forbidden(&mut s, "comment.edit", json!({"canvas": common::canvas(), "id": id(20), "text": "Cy's now"}));
    s.dispatch("comment.move", &json!({"canvas": common::canvas(), "id": id(20), "pos": {"x": 5, "y": 5}})).unwrap();
    s.dispatch("comment.remove", &json!({"canvas": common::canvas(), "id": id(20)})).unwrap();
    s.dispatch("node.add", &json!({"canvas": common::canvas(), "node": common::node(5)})).unwrap();
}

#[test]
fn a_discussion_on_a_removed_card_stays_and_comes_back_pinned_on_undo() {
    let mut s = Session::new(common::project()).unwrap();
    let mut pinned = note(10, "This card");
    pinned["node"] = json!(common::node_id(1));
    s.dispatch("comment.create", &pinned).unwrap();
    s.dispatch("node.remove", &json!({"canvas": common::canvas(), "id": common::node_id(1)})).unwrap();
    let kept = thread(&s, 10);
    assert_eq!(kept.node, None);
    assert_eq!(kept.text, "This card");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(thread(&s, 10).node, Some(common::node_id(1)));
}

#[test]
fn share_links_are_managed_through_the_sharing_host() {
    let mut s = Session::new(common::project()).unwrap();
    let create = json!({"role": "comment", "name": "Ana"});
    assert_eq!(s.dispatch("share.link_create", &create).unwrap_err().kind, ErrorKind::Unsupported);
    for (command, params) in [
        ("share.link_create", json!({"role": "owner", "name": "Ana"})),
        ("share.link_create", json!({"role": "view", "name": " "})),
        ("share.link_update", json!({"id": "nope", "role": "view", "name": "Ana"})),
        ("share.link_revoke", json!({"id": "A".repeat(64)})),
    ] {
        assert_eq!(s.dispatch(command, &params).unwrap_err().kind, ErrorKind::InvalidInput, "{params}");
    }

    let collab = Rc::new(RefCell::new(Collab::share(&s, RoomId::from_bytes([5; 32])).unwrap()));
    let host = Rc::clone(&collab);
    s.register_host_service(LINKS_SERVICE, move |request| host.borrow_mut().links_request(request, 1_800_000_000)).unwrap();
    let before = s.revision();
    let made = s.dispatch("share.link_create", &create).unwrap();
    assert_eq!(made["key"].as_str().unwrap().len(), 64);
    assert_ne!(made["key"], made["id"]);
    let listed = s.dispatch("share.links", &json!({})).unwrap();
    assert_eq!(listed["links"], json!([{"id": made["id"], "role": "comment", "name": "Ana", "expires": null, "expired": false}]));
    let changed = s.dispatch("share.link_update", &json!({"id": made["id"], "role": "view", "name": "Ana", "expires": 1_900_000_000})).unwrap();
    assert_eq!(changed["role"], "view");
    s.dispatch("share.link_revoke", &json!({"id": made["id"]})).unwrap();
    assert_eq!(s.dispatch("share.link_revoke", &json!({"id": made["id"]})).unwrap_err().kind, ErrorKind::NotFound);
    // Links aren't part of the document.
    assert_eq!(s.revision(), before);
    let frames = collab.borrow_mut().publish(&s).unwrap();
    assert_eq!(frames.len(), 3);
}
