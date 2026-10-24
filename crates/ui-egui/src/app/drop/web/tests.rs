use super::super::super::tests::harness;
use super::*;
use egui_kittest::Harness;

const MIB: f64 = 1024.0 * 1024.0;

fn nodes(h: &Harness<'static, NodeCraftApp>) -> Vec<nodecraft_graph::Node> {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().cloned().collect()
}
/// A browser drop as `start_web_drop` leaves it, with `reads` as the tasks fill them.
fn pending(h: &mut Harness<'static, NodeCraftApp>, reads: Vec<(&str, Slot)>, skipped: Vec<(&str, &str)>) -> Rc<RefCell<Vec<(String, Slot)>>> {
    let deadline = h.ctx.input(|i| i.time) + READ_SECONDS;
    let app = h.state_mut();
    let reads = Rc::new(RefCell::new(reads.into_iter().map(|(n, s)| (n.to_owned(), s)).collect::<Vec<_>>()));
    let identity = crate::navigation::Geometry::identity(&mut app.session);
    app.web_drop = Some(Pending {
        reads: Rc::clone(&reads),
        skipped: skipped.into_iter().map(|(n, w)| (n.to_owned(), w.to_owned())).collect(),
        identity,
        project: app.session.document().id(),
        canvas: app.canvas,
        revision: app.session.revision(),
        position: Position { x: 100.0, y: 100.0 },
        step: 300.0,
        deadline,
    });
    reads
}

#[test]
fn files_are_admitted_by_reported_size_count_and_total_before_any_read() {
    let skip = |why: &str| Admission::Skip(why.to_owned());
    assert_eq!(
        admit([Some(10.0), None, Some(f64::NAN), Some(-1.0), Some(16.0 * MIB + 1.0), Some(f64::INFINITY), Some(0.0)]),
        [
            Admission::Read,
            skip("the file can't be read"),
            skip("the file can't be read"),
            skip("the file can't be read"),
            skip("files over 16 MiB can't be added"),
            skip("the file can't be read"),
            Admission::Read,
        ]
    );
    let total = (MAX_PROJECT_MEDIA_BYTES as f64 / MAX_MEDIA_BYTES as f64).ceil() as usize;
    let big = admit(std::iter::repeat_n(Some(MAX_MEDIA_BYTES as f64), total + 1));
    assert!(big[..total - 1].iter().all(|a| *a == Admission::Read));
    assert!(matches!(big.last(), Some(Admission::Skip(why)) if why.starts_with("a drop reads at most")));
    let many = admit(std::iter::repeat_n(Some(1.0), MAX_FILES + 2));
    assert_eq!(many.iter().filter(|a| **a == Admission::Read).count(), MAX_FILES);
    assert_eq!(many.last(), Some(&skip("at most 32 files are added at once")));
    assert_eq!(name(std::path::Path::new(&"é".repeat(200))).chars().count(), 80);
    assert_eq!(name(std::path::Path::new("")), "A dropped file");
}

#[test]
fn a_browser_drop_waits_for_every_read_then_imports_as_one_undo_step() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let png = nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap();
    let reads = pending(&mut h, vec![("note.txt", Some(Ok(b"Dropped words".to_vec()))), ("pic.png", None)], vec![]);
    h.run();
    assert!(h.state().web_drop.is_some(), "a read still running keeps the drop");
    assert_eq!(h.state().session.document(), &before);
    reads.borrow_mut()[1].1 = Some(Ok(png));
    h.run();
    assert!(h.state().web_drop.is_none());
    let existing = before.canvases().get(&h.state().canvas).unwrap().nodes().clone();
    let added: Vec<_> = nodes(&h).into_iter().filter(|n| !existing.contains_key(&n.id)).collect();
    assert_eq!(added.len(), 2, "{:?}", h.state().notice);
    assert!(added.iter().any(|n| n.kind == "source.image") && added.iter().any(|n| n.kind == "source.text"));
    assert_eq!(h.state().selection.nodes(), &added.iter().map(|n| n.id).collect());
    assert!(h.state().notice.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo removes the whole drop");
}

#[test]
fn skipped_failed_and_grown_files_are_named_and_the_rest_still_land() {
    let mut h = harness();
    let count = nodes(&h).len();
    pending(
        &mut h,
        vec![
            ("broken.txt", Some(Err("NotReadableError".into()))),
            ("grew.bin", Some(Ok(vec![0; MAX_MEDIA_BYTES + 1]))),
            ("fine.txt", Some(Ok(b"words".to_vec()))),
        ],
        vec![("huge.mov", "files over 16 MiB can't be added")],
    );
    h.run();
    assert_eq!(nodes(&h).len(), count + 1);
    assert_eq!(h.state().notice.as_deref(), Some("huge.mov wasn't added: files over 16 MiB can't be added (and 2 more files weren't)."));
    // Only skips: nothing is imported, and the notice says why.
    pending(&mut h, vec![], vec![("folder", "the file can't be read")]);
    h.run();
    assert_eq!(h.state().notice.as_deref(), Some("folder wasn't added: the file can't be read."));
    assert_eq!(nodes(&h).len(), count + 1);
}

#[test]
fn an_edit_while_files_are_read_wins() {
    let mut h = harness();
    let reads = pending(&mut h, vec![("note.txt", None)], vec![]);
    let canvas = h.state().canvas;
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([78; 16]), "fixture.unknown");
    h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": node})).unwrap();
    let count = nodes(&h).len();
    reads.borrow_mut()[0].1 = Some(Ok(b"late".to_vec()));
    h.run();
    assert_eq!(nodes(&h).len(), count);
    assert_eq!(h.state().notice.as_deref(), Some("The document changed while reading files; nothing was added. Drop them again."));
    assert!(h.state().web_drop.is_none());
}

#[test]
fn a_stuck_browser_read_expires_and_a_late_result_cannot_import() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let reads = pending(&mut h, vec![("late.txt", None)], vec![]);
    h.state_mut().web_drop.as_mut().unwrap().deadline = -1.0;
    h.step();
    assert!(!h.state().drop_pending());
    assert_eq!(h.state().notice.as_deref(), Some("The browser didn't finish reading files; nothing was added. Drop them again."));
    reads.borrow_mut()[0].1 = Some(Ok(b"too late".to_vec()));
    h.run();
    assert_eq!(h.state().session.document(), &before);
    pending(&mut h, vec![("fresh.txt", Some(Ok(b"fresh read".to_vec())))], vec![]);
    h.run();
    assert_eq!(nodes(&h).len(), before.canvases().get(&h.state().canvas).unwrap().nodes().len() + 1);
    assert!(h.state().notice.is_none());
}

#[test]
fn escape_cancels_a_pending_browser_read_without_clearing_selection_or_accepting_late_bytes() {
    let mut h = harness();
    let first = nodes(&h).first().unwrap().id;
    h.state_mut().selection.replace([first].into());
    let before = h.state().session.document().clone();
    let reads = pending(&mut h, vec![("late.txt", None)], vec![]);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().drop_pending());
    assert!(h.state().selection.nodes().contains(&first), "the pending read owns this Escape");
    assert_eq!(h.state().notice.as_deref(), Some("File reading was cancelled; nothing was added."));
    reads.borrow_mut()[0].1 = Some(Ok(b"too late".to_vec()));
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn a_document_replacement_cancels_even_a_browser_read_that_never_settles() {
    let mut h = harness();
    pending(&mut h, vec![("never.txt", None)], vec![]);
    let canvas = h.state().canvas;
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([79; 16]), "fixture.unknown");
    h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas":canvas,"node":node})).unwrap();
    h.step();
    assert!(h.state().web_drop.is_none());
    assert_eq!(h.state().notice.as_deref(), Some("The document changed while reading files; nothing was added. Drop them again."));
}

#[test]
fn escape_with_a_search_button_focused_closes_search_and_preserves_the_pending_read() {
    use egui_kittest::kittest::Queryable;
    let mut h = harness();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert!(h.state().search.open);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Close search").focus();
    h.run();
    assert!(!crate::canvas::text_input_focused(&h.ctx));
    let before = h.state().session.document().clone();
    let reads = pending(&mut h, vec![("public.png", None)], vec![]);
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().search.open);
    assert!(h.state().web_drop.is_some(), "Escape belongs to search, not the drop");
    assert_eq!(h.state().session.document(), &before);
    reads.borrow_mut()[0].1 = Some(Err("fixture ended".into()));
    h.run();
    assert!(h.state().web_drop.is_none());
}
