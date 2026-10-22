use super::*;
use nodecraft_graph::{Change, Changeset, Endpoint, Port};
use nodecraft_types::{EdgeId, Modality, NodeId, PortType};
use proptest::prelude::*;

fn pid() -> ProjectId {
    ProjectId::from_bytes([1; 16])
}
fn cid() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}
fn nid(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn card(n: u8) -> Node {
    let mut node = Node::new(nid(n), "test.text");
    node.title = format!("Card {n}");
    node.inputs.push(Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
    node.params.insert("prompt".into(), Value::Text("a fox".into()));
    node
}

fn edge(n: u8, from: u8, to: u8) -> Edge {
    Edge {
        id: EdgeId::from_bytes([n; 16]),
        from: Endpoint { node: nid(from), port: "out".into() },
        to: Endpoint { node: nid(to), port: "in".into() },
        order: 0,
    }
}

fn edit(project: &mut Project, changes: Vec<Change>) {
    Changeset::new(changes).apply(project).unwrap();
}

/// A project with one canvas holding cards 1, 2 and 3.
fn start() -> Project {
    let mut project = Project::new(pid(), "Shared").unwrap();
    edit(&mut project, vec![Change::AddCanvas { canvas: Canvas::new(cid(), "Main") }]);
    edit(&mut project, (1..=3).map(|n| Change::AddNode { canvas: cid(), node: card(n) }).collect());
    project
}

/// Brings two copies level, both ways.
fn exchange(a: &mut Mirror, b: &mut Mirror) {
    let to_b = a.update_for(&b.state_vector().unwrap()).unwrap();
    let to_a = b.update_for(&a.state_vector().unwrap()).unwrap();
    b.receive(&to_b).unwrap();
    a.receive(&to_a).unwrap();
}

/// What a peer takes in: the materialized project, with its repairs sent to `other`.
fn take(mine: &mut Mirror, other: &mut Mirror) -> Project {
    let Materialized { project, repairs, .. } = mine.materialize().unwrap().unwrap();
    if let Some(repairs) = repairs {
        other.receive(&repairs).unwrap();
    }
    mine.adopt(&project);
    project
}

fn joined(project: &Project) -> (Mirror, Mirror) {
    let (a, update) = Mirror::share(project).unwrap();
    let mut b = Mirror::join();
    assert!(b.materialize().unwrap().is_none());
    b.receive(&update).unwrap();
    (a, b)
}

#[test]
fn a_joiner_gets_the_whole_project() {
    let mut project = start();
    edit(&mut project, vec![Change::Connect { canvas: cid(), edge: edge(1, 1, 2) }]);
    let (_, mut b) = joined(&project);
    assert_eq!(b.materialize().unwrap().unwrap().project, project);
}

#[test]
fn a_move_and_a_prompt_edit_on_one_card_both_stay() {
    let project = start();
    let (mut a, mut b) = joined(&project);
    b.adopt(&project);
    let mut here = project.clone();
    edit(&mut here, vec![Change::MoveNode { canvas: cid(), id: nid(1), pos: Position { x: 50.0, y: 9.0 } }]);
    let mut there = project.clone();
    edit(&mut there, vec![Change::SetParam { canvas: cid(), id: nid(1), name: "prompt".into(), value: Some(Value::Text("a hare".into())) }]);
    a.publish(&here).unwrap().unwrap();
    b.publish(&there).unwrap().unwrap();
    exchange(&mut a, &mut b);
    let (x, y) = (take(&mut a, &mut b), take(&mut b, &mut a));
    assert_eq!(x, y);
    let node = &x.canvases()[&cid()].nodes()[&nid(1)];
    assert_eq!(node.pos, Position { x: 50.0, y: 9.0 });
    assert_eq!(node.params.get("prompt"), Some(&Value::Text("a hare".into())));
}

#[test]
fn two_wires_into_one_input_end_as_one_on_both_sides() {
    let project = start();
    let (mut a, mut b) = joined(&project);
    b.adopt(&project);
    let mut here = project.clone();
    edit(&mut here, vec![Change::Connect { canvas: cid(), edge: edge(7, 1, 3) }]);
    let mut there = project.clone();
    edit(&mut there, vec![Change::Connect { canvas: cid(), edge: edge(4, 2, 3) }]);
    a.publish(&here).unwrap();
    b.publish(&there).unwrap();
    exchange(&mut a, &mut b);
    let mut x = a.materialize().unwrap().unwrap();
    assert_eq!(x.dropped, 1);
    // The repair travels, so the other side holds the same entries afterwards.
    b.receive(&x.repairs.take().unwrap()).unwrap();
    let y = b.materialize().unwrap().unwrap();
    assert_eq!(y.dropped, 0);
    assert_eq!(x.project, y.project);
    assert_eq!(x.project.canvases()[&cid()].edges().keys().copied().collect::<Vec<_>>(), vec![EdgeId::from_bytes([4; 16])]);
}

#[test]
fn a_removed_card_takes_its_wires_and_entries_with_it() {
    let mut project = start();
    edit(&mut project, vec![Change::Connect { canvas: cid(), edge: edge(1, 1, 2) }]);
    let (mut a, mut b) = joined(&project);
    b.adopt(&project);
    // One peer removes card 2 while the other wires card 3 into it.
    let mut here = project.clone();
    edit(&mut here, vec![Change::RemoveNode { canvas: cid(), id: nid(2) }]);
    let mut there = project.clone();
    edit(&mut there, vec![Change::Disconnect { canvas: cid(), id: EdgeId::from_bytes([1; 16]) }]);
    edit(&mut there, vec![Change::Connect { canvas: cid(), edge: edge(5, 3, 2) }]);
    a.publish(&here).unwrap();
    b.publish(&there).unwrap();
    exchange(&mut a, &mut b);
    let x = take(&mut a, &mut b);
    let y = take(&mut b, &mut a);
    assert_eq!(x, y);
    assert!(!x.canvases()[&cid()].nodes().contains_key(&nid(2)));
    assert!(x.canvases()[&cid()].edges().is_empty());
    x.validate().unwrap();
}

#[test]
fn nothing_changed_publishes_nothing() {
    let project = start();
    let (mut a, _) = Mirror::share(&project).unwrap();
    assert_eq!(a.publish(&project).unwrap(), None);
    let mut other = project.clone();
    edit(&mut other, vec![Change::SetProjectTitle { title: "Renamed".into() }]);
    assert!(a.publish(&other).unwrap().is_some());
    // A different project can't go into this document.
    assert!(a.publish(&Project::new(ProjectId::from_bytes([9; 16]), "").unwrap()).is_err());
}

#[test]
fn unreadable_entries_and_updates_are_refused_not_kept() {
    let project = start();
    let (mut a, _) = Mirror::share(&project).unwrap();
    assert!(a.receive(&[0xff, 0x00, 0x13]).is_err());
    assert!(a.update_for(&[0xff; 9]).is_err());
    // An entry a hostile peer wrote under a card's key that isn't a card.
    let mut hostile = Mirror::join();
    hostile.receive(&a.update_for(&[0]).unwrap()).unwrap();
    let garbage = hostile
        .write(|txn| {
            let nodes = txn.get_or_insert_map(NODES);
            nodes.insert(txn, in_canvas(cid(), nid(9)), Any::String(Arc::from("{not json")));
            nodes.insert(txn, "nonsense", Any::Bool(true));
            Ok(())
        })
        .unwrap();
    a.receive(&garbage).unwrap();
    let materialized = a.materialize().unwrap().unwrap();
    assert_eq!(materialized.project, project);
    // The cleanup removes both entries for everyone.
    hostile.receive(&materialized.repairs.unwrap()).unwrap();
    assert!(hostile.read().unwrap().get_map(NODES).unwrap().get(&hostile.read().unwrap(), "nonsense").is_none());
}

/// One random edit a peer makes to its copy, valid or not (an invalid one is skipped).
fn change(pick: (u8, u8, u8, u8)) -> Change {
    let (kind, a, b, c) = pick;
    let (a, b) = (a % 6 + 1, b % 6 + 1);
    match kind % 6 {
        0 => Change::AddNode { canvas: cid(), node: card(a) },
        1 => Change::RemoveNode { canvas: cid(), id: nid(a) },
        2 => Change::MoveNode { canvas: cid(), id: nid(a), pos: Position { x: f32::from(c), y: 0.0 } },
        3 => Change::SetParam { canvas: cid(), id: nid(a), name: "prompt".into(), value: Some(Value::Text(format!("v{c}"))) },
        4 => Change::Connect { canvas: cid(), edge: edge(c, a, b) },
        _ => Change::Disconnect { canvas: cid(), id: EdgeId::from_bytes([c; 16]) },
    }
}

fn edited(project: &Project, picks: &[(u8, u8, u8, u8)]) -> Project {
    let mut project = project.clone();
    for pick in picks {
        let mut next = project.clone();
        if Changeset::new(vec![change(*pick)]).apply(&mut next).is_ok() && next.validate().is_ok() {
            project = next;
        }
    }
    project
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn two_peers_editing_at_once_end_with_one_valid_project(
        mine in proptest::collection::vec(any::<(u8, u8, u8, u8)>(), 0..12),
        theirs in proptest::collection::vec(any::<(u8, u8, u8, u8)>(), 0..12),
    ) {
        let project = start();
        let (mut a, mut b) = joined(&project);
        b.adopt(&project);
        a.publish(&edited(&project, &mine)).unwrap();
        b.publish(&edited(&project, &theirs)).unwrap();
        exchange(&mut a, &mut b);
        let x = take(&mut a, &mut b);
        let y = take(&mut b, &mut a);
        x.validate().unwrap();
        // Both took the other's repairs; another round changes nothing.
        let (x2, y2) = (take(&mut a, &mut b), take(&mut b, &mut a));
        prop_assert_eq!(&x2, &y2);
        prop_assert_eq!(&x, &x2);
        prop_assert_eq!(y, y2);
    }
}
