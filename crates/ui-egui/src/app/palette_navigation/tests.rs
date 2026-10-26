use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::navigation_cmds::NavigationContext;

fn harness() -> Harness<'static, NodeCraftApp> {
    super::super::tests::harness()
}

fn open_palette(h: &mut Harness<'static, NodeCraftApp>, query: &str) {
    let at = Pos2::new(80.0, 450.0);
    h.hover_at(at);
    for _ in 0..30 {
        h.step();
    }
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
    }
    h.run();
    assert!(h.state().palette.is_some());
    h.event(egui::Event::Text(query.into()));
    h.run();
    assert!(Palette::owns_text_focus(&h.ctx));
}

fn offers(h: &mut Harness<'static, NodeCraftApp>, id: &'static str) -> bool {
    let ctx = h.ctx.clone();
    h.state_mut().live_palette_entries(&ctx).iter().any(|entry| entry.target == palette::Target::Command(id))
}

#[test]
fn minimap_palette_rows_run_by_enter_and_pointer_without_document_edits() {
    let mut h = harness();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let visible = h.state().navigation.visible;
    for click in [false, true] {
        open_palette(&mut h, "Toggle minimap");
        let ctx = h.ctx.clone();
        assert!(offers(&mut h, "canvas.minimap_toggle"));
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.minimap_toggle", &json!({})).is_err(), "other surfaces do not borrow palette focus");
        assert!(!offers(&mut h, "canvas.pan_to"), "commands needing parameters remain absent");
        if click {
            h.get_by_label_contains("Toggle minimap").click();
        } else {
            h.key_press(egui::Key::Enter);
        }
        h.run();
        assert!(h.state().palette.is_none(), "palette selection must close for pointer={click}");
        assert_eq!(h.state().navigation.visible, if click { visible } else { !visible });
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().session.events_since(0).unwrap(), events);
        assert!(!h.state().session.can_undo() && !h.state().session.can_redo());
        assert!(h.state_mut().session.swap_resource::<NavigationContext>(None).is_none());
        assert_eq!(h.state_mut().session.dispatch("canvas.minimap_toggle", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    }
}

#[test]
fn palette_focus_exception_does_not_override_another_editor_or_gesture() {
    let mut h = harness();
    let ctx = h.ctx.clone();
    open_palette(&mut h, "Toggle minimap");
    let visible = h.state().navigation.visible;
    ctx.set_dragged_id(egui::Id::new("external-gesture"));
    assert!(!offers(&mut h, "canvas.minimap_toggle"));
    h.state_mut().dispatch_live_palette_command(&ctx, "canvas.minimap_toggle");
    assert_eq!(h.state().navigation.visible, visible);
    ctx.stop_dragging();

    let editor = egui::Id::new("external-text-editor");
    egui::text_edit::TextEditState::default().store(&ctx, editor);
    ctx.memory_mut(|memory| memory.request_focus(editor));
    assert!(canvas::text_input_focused(&ctx));
    assert!(!offers(&mut h, "canvas.minimap_toggle"));
    h.state_mut().dispatch_live_palette_command(&ctx, "canvas.minimap_toggle");
    assert!(ctx.memory(|memory| memory.has_focus(editor)));
    assert_eq!(h.state().navigation.visible, visible);
    assert_eq!(h.state().session.revision(), 0);
    assert!(h.state_mut().session.swap_resource::<NavigationContext>(None).is_none());
}

#[test]
fn palette_fit_uses_current_selection_and_preserves_existing_history() {
    let mut h = harness();
    let canvas = h.state().canvas;
    let id = *h.state().session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":"A public example"})).unwrap();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    open_palette(&mut h, "Fit selected nodes");
    assert!(!offers(&mut h, "canvas.fit_selection"));
    h.state_mut().selection.only(id);
    h.run();
    assert!(offers(&mut h, "canvas.fit_selection"));
    let before = h.state().view;
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().palette.is_none());
    assert_ne!(h.state().view, before);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert!(h.state().session.can_undo());
}

#[test]
#[ignore = "manual public-safe palette capture; requires NODECRAFT_PALETTE762_EVIDENCE"]
fn render_palette_navigation_evidence() {
    let root = std::path::PathBuf::from(std::env::var("NODECRAFT_PALETTE762_EVIDENCE").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::builder()
        .wgpu()
        .with_step_dt(1.0 / 60.0)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    open_palette(&mut h, "Toggle minimap");
    h.render().unwrap().save(root.join("palette.png")).unwrap();
    h.key_press(egui::Key::Enter);
    h.run();
    h.render().unwrap().save(root.join("chosen.png")).unwrap();
}
