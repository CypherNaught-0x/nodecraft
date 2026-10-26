use super::super::tests::harness;
use super::*;
use egui_kittest::Harness;

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(4, 4, &[90; 64], &Default::default()).unwrap()
}
fn count(h: &Harness<'static, NodeCraftApp>) -> usize {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().len()
}
fn gesture(h: &mut Harness<'static, NodeCraftApp>) {
    h.hover_at(egui::pos2(600.0, 300.0));
    h.run();
    // A real DOM paste supplies this bounded inbox signal. Key input still passes the actual
    // app's focus/gesture admission; the browser proof covers the DOM and permission boundary.
    h.state().web_paste.inbox.set(true);
    h.event(egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.step();
}
fn reply(h: &mut Harness<'static, NodeCraftApp>, result: Result<Vec<u8>, String>) -> Rc<Cell<usize>> {
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    h.state_mut().web_paste.reader = Some(Rc::new(move |state, busy| {
        counted.set(counted.get() + 1);
        ReadState::complete(&state, result.clone());
        busy.set(false);
    }));
    calls
}

#[test]
fn a_browser_png_paste_uses_the_desktop_import_and_one_undo_step() {
    let mut h = harness();
    let calls = reply(&mut h, Ok(png()));
    let before = h.state().session.document().clone();
    let old = count(&h);
    gesture(&mut h);
    h.run();
    assert_eq!(calls.get(), 1);
    assert_eq!(count(&h), old + 1, "{:?}", h.state().notice);
    let selected = h.state().selection.nodes();
    assert_eq!(selected.len(), 1);
    let id = selected.iter().next().unwrap();
    assert_eq!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(id).unwrap().kind, "source.image");
    let node = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(id).unwrap();
    let world = h.state().view.to_world(h.state().viewport.min, egui::pos2(600.0, 300.0));
    let rect = super::super::payload_rect(&h.state().session, node, h.state().theme.tokens());
    assert!((rect.center() - world).length() < 1.0, "browser PNG uses the current payload centering");
    assert!(h.state().notice.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn a_completed_read_survives_a_browser_clipboard_frame_after_the_deadline() {
    let mut h = harness();
    let before = h.state().session.document().clone();
    let old = count(&h);
    let waiting = Rc::new(RefCell::new(None));
    let capture = Rc::clone(&waiting);
    h.state_mut().web_paste.reader = Some(Rc::new(move |state, busy| {
        *capture.borrow_mut() = Some((state, busy));
    }));
    gesture(&mut h);
    let deadline = h.state().web_paste.pending.as_ref().unwrap().deadline;
    let (state, busy) = waiting.borrow_mut().take().unwrap();
    ReadState::complete(&state, Ok(png()));
    busy.set(false);
    // The read is done, but a background tab receives no egui frame until after its deadline.
    h.input_mut().time = Some(deadline + READ_SECONDS);
    h.step();
    assert!(h.ctx.input(|i| i.time) > deadline);
    assert_eq!(count(&h), old + 1, "completed PNG must survive the delayed frame: {:?}", h.state().notice);
    assert!(h.state().web_paste.pending.is_none());
    assert!(!h.state().web_paste.busy.get());
    assert!(h.state().notice.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn denied_missing_and_malformed_browser_images_refuse_without_a_mutation() {
    for result in [Err(DENIED.to_owned()), Err(NO_PNG.to_owned()), Ok(b"not a PNG".to_vec())] {
        let mut h = harness();
        let before = h.state().session.document().clone();
        let calls = reply(&mut h, result);
        gesture(&mut h);
        h.run();
        assert_eq!(calls.get(), 1);
        assert_eq!(h.state().session.document(), &before);
        assert!(h.state().notice.is_some());
        assert!(!h.state().session.can_undo());
    }
}

#[test]
fn an_image_gesture_while_keys_are_open_cannot_read_or_be_replayed_after_the_panel_closes() {
    let mut h = harness();
    let calls = reply(&mut h, Ok(png()));
    let before = h.state().session.document().clone();
    h.state_mut().keys.open = true;
    gesture(&mut h);
    h.run();
    h.state_mut().keys.open = false;
    h.run();
    assert_eq!(calls.get(), 0);
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn text_paste_keeps_its_existing_precedence_and_never_reads_browser_clipboard_bytes() {
    let mut h = harness();
    let calls = reply(&mut h, Ok(png()));
    let old = count(&h);
    h.state().web_paste.inbox.set(true);
    h.event(egui::Event::Paste("Public pasted words".into()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::V);
    h.run();
    assert_eq!(calls.get(), 0);
    assert_eq!(count(&h), old + 1);
    assert!(
        h.state()
            .session
            .document()
            .canvases()
            .get(&h.state().canvas)
            .unwrap()
            .nodes()
            .values()
            .any(|node| { node.kind == "source.text" && node.params.get("text") == Some(&nodecraft_types::Value::Text("Public pasted words".into())) })
    );
}

#[test]
fn a_late_permission_result_cannot_reach_an_edited_document() {
    let mut h = harness();
    let waiting = Rc::new(RefCell::new(None));
    let capture = Rc::clone(&waiting);
    h.state_mut().web_paste.reader = Some(Rc::new(move |state, busy| {
        *capture.borrow_mut() = Some((state, busy));
    }));
    gesture(&mut h);
    assert!(h.state().web_paste.pending.is_some());
    let canvas = h.state().canvas;
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([80; 16]), "fixture.unknown");
    h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas":canvas,"node":node})).unwrap();
    h.step();
    let before = h.state().session.document().clone();
    assert!(h.state().web_paste.pending.is_none());
    let (state, busy) = waiting.borrow_mut().take().unwrap();
    ReadState::complete(&state, Ok(png()));
    busy.set(false);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn an_unsettled_permission_times_out_without_replacement_requests_or_blocking_canvas_keys() {
    let mut h = harness();
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    h.state_mut().web_paste.reader = Some(Rc::new(move |_, _| {
        counted.set(counted.get() + 1);
    }));
    gesture(&mut h);
    h.state_mut().web_paste.pending.as_mut().unwrap().deadline = -1.0;
    h.step();
    assert!(h.state().web_paste.pending.is_none());
    assert!(h.state().web_paste.busy.get());
    gesture(&mut h);
    assert_eq!(calls.get(), 1, "an unresolved permission request is never replaced by another one");
    assert_eq!(h.state().notice.as_deref(), Some(READING));
    let old = count(&h);
    h.hover_at(egui::pos2(600.0, 300.0));
    h.key_press(egui::Key::T);
    h.run();
    assert_eq!(count(&h), old + 1, "ordinary canvas input remains available");
}

#[test]
fn a_paste_on_a_sheet_is_drained_before_return_and_cannot_replay_on_a_canvas() {
    let mut h = harness();
    let calls = reply(&mut h, Ok(png()));
    let canvas = h.state().canvas;
    let sheet = nodecraft_types::CanvasId::from_bytes([92; 16]);
    h.state_mut().session.dispatch("sheet.create", &serde_json::json!({"canvas":sheet,"title":"Public sheet"})).unwrap();
    h.state_mut().open_canvas(sheet);
    h.run();
    assert!(h.state().on_sheet());
    let before = h.state().session.document().clone();
    h.state().web_paste.inbox.set(true);
    h.step();
    assert!(!h.state().web_paste.inbox.get());
    assert!(!h.state().web_paste.requested);
    h.state_mut().open_canvas(canvas);
    h.hover_at(egui::pos2(600.0, 300.0));
    h.run();
    assert_eq!(calls.get(), 0);
    assert_eq!(h.state().session.document(), &before);
}
