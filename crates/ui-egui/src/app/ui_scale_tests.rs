//! #1107: the interface size grows the shell's chrome, its shortcuts leave canvas zoom alone, and
//! the setting survives a restart and a damaged value.
use super::*;
use crate::app::panel_check::{check_open_panels, check_question};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::ParamValues;

const C: CanvasId = CanvasId::from_bytes([107; 16]);
const A: NodeId = NodeId::from_bytes([7; 16]);
/// The desktop app's default window is larger; the issue asks for 200 % to fit this one.
const WINDOW: Vec2 = Vec2::new(1280.0, 720.0);

/// One image card whose model can be changed, so the Review model change question can open.
fn app() -> NodeCraftApp {
    let mut project = Project::new(ProjectId::from_bytes([107; 16]), "Interface size").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let values: ParamValues = [("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("Two circles".into()))].into();
    let node = session.kinds().get("generation.image").unwrap().instantiate_instance(A, Default::default(), &values).unwrap();
    session.dispatch("node.add", &json!({"canvas":C,"node":node})).unwrap();
    NodeCraftApp::from_session(session, C).unwrap()
}

fn harness_with(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    let mut h = Harness::builder().with_size(WINDOW).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn set_size(h: &mut Harness<'static, NodeCraftApp>, percent: u16) {
    h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent": percent})).unwrap();
    h.run();
    let zoom = f32::from(percent) / 100.0;
    assert!((h.ctx.zoom_factor() - zoom).abs() < 1e-4, "{percent} %: zoom factor {}", h.ctx.zoom_factor());
    let screen = h.ctx.content_rect();
    assert!((screen.width() - WINDOW.x / zoom).abs() < 1.0 && (screen.height() - WINDOW.y / zoom).abs() < 1.0, "{percent} %: {screen:?}");
}

#[test]
fn at_150_and_200_percent_the_palette_file_panel_and_a_question_fit_the_window() {
    for percent in [150, 200] {
        let mut h = harness_with(app());
        set_size(&mut h, percent);
        let screen = h.ctx.content_rect();
        let bar = h.state().topbar_rect();
        assert!(screen.expand(1.0).contains_rect(bar), "{percent} %: the top bar {bar:?} leaves {screen:?}");
        assert!(h.query_by_role_and_label(Role::Button, "File").is_some(), "{percent} %: File stays in the row");

        // The quick-add palette, opened at the bottom right of the canvas, stays on the screen.
        let corner = h.state().viewport.max - Vec2::splat(4.0);
        h.state_mut().palette = Some(Palette::new(Pos2::ZERO, corner));
        h.run();
        assert!(h.state().palette.is_some(), "{percent} %: the palette opens");
        let palette = egui::AreaState::load(&h.ctx, egui::Id::new("quick_add_palette")).unwrap().rect();
        assert!(screen.expand(1.0).contains_rect(palette), "{percent} %: the palette {palette:?} leaves {screen:?}");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().palette.is_none());

        // Project files, a floating panel under the File button.
        h.get_by_role_and_label(Role::Button, "File").click();
        h.run_steps(4);
        check_open_panels(&h, &format!("Project files at {percent} %"));
        h.key_press(egui::Key::Escape);
        h.run();

        // A question: Review model change.
        // The card's top at the top of the canvas, at canvas zoom 100 %: its model control shows.
        let pos = h.state().session.document().canvases().get(&C).unwrap().nodes().get(&A).unwrap().pos;
        let (viewport, zoom) = (h.state().viewport, h.state().view.zoom);
        let (x, y) = (f64::from(pos.x) + f64::from(viewport.width() / zoom) / 2.0 - 20.0, f64::from(pos.y) + f64::from(viewport.height() / zoom) / 2.0 - 20.0);
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.pan_to", &json!({"x": x, "y": y})).unwrap();
        h.run();
        h.get_by_label("Change model…").click();
        h.run();
        h.event(egui::Event::Text("openai-images.flare".into()));
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        check_question(&h, egui::Id::new("nodecraft.model_review"), &format!("Review model change at {percent} %"));
    }
}

#[test]
fn the_shortcuts_size_the_interface_and_leave_canvas_zoom_alone() {
    let mut h = harness_with(app());
    let ctrl_alt = egui::Modifiers::COMMAND | egui::Modifiers::ALT;
    let zoom = h.state().view.zoom;
    h.key_press_modifiers(ctrl_alt, egui::Key::Equals);
    h.run();
    assert_eq!(h.state().interface_size().percent(), 110);
    assert!((h.ctx.zoom_factor() - 1.1).abs() < 1e-4);
    h.key_press_modifiers(ctrl_alt, egui::Key::Plus);
    h.run();
    assert_eq!(h.state().interface_size().percent(), 120);
    h.key_press_modifiers(ctrl_alt, egui::Key::Minus);
    h.run();
    assert_eq!(h.state().interface_size().percent(), 110);
    assert_eq!(h.state().view.zoom, zoom, "Ctrl+Alt keys never zoom the canvas");
    h.key_press_modifiers(ctrl_alt, egui::Key::Num0);
    h.run();
    assert_eq!(h.state().interface_size().percent(), 100);
    assert_eq!(h.ctx.zoom_factor(), 1.0);
    // Ctrl+= still zooms the canvas, and only the canvas.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Equals);
    h.run();
    assert!(h.state().view.zoom > zoom);
    assert_eq!(h.state().interface_size().percent(), 100);
    // At the largest size the key is still taken: nothing zooms and no notice is raised.
    h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent": 200})).unwrap();
    h.run();
    let zoom = h.state().view.zoom;
    h.key_press_modifiers(ctrl_alt, egui::Key::Equals);
    h.run();
    assert_eq!(h.state().interface_size().percent(), 200);
    assert_eq!(h.state().view.zoom, zoom);
    assert!(h.state().notice.is_none(), "{:?}", h.state().notice);
}

/// AltGr is Ctrl+Alt: on a Swedish layout AltGr with the physical `-` key types `\`, which must
/// not run Smaller interface. Only the digit matches by its physical key.
#[test]
fn an_altgr_character_on_the_minus_key_does_nothing() {
    let mut h = harness_with(app());
    let zoom = h.state().view.zoom;
    let modifiers = egui::Modifiers::COMMAND | egui::Modifiers::ALT;
    h.event(egui::Event::Key { key: egui::Key::Backslash, physical_key: Some(egui::Key::Minus), pressed: true, repeat: false, modifiers });
    h.event(egui::Event::Key { key: egui::Key::Backslash, physical_key: Some(egui::Key::Minus), pressed: false, repeat: false, modifiers });
    h.run();
    assert_eq!(h.state().interface_size().percent(), 100);
    assert_eq!(h.state().view.zoom, zoom);
    // The digit row's 0 still resets from its physical key, whatever character the layout gives it.
    h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent": 150})).unwrap();
    h.run();
    h.event(egui::Event::Key { key: egui::Key::CloseCurlyBracket, physical_key: Some(egui::Key::Num0), pressed: true, repeat: false, modifiers });
    h.run();
    assert_eq!(h.state().interface_size().percent(), 100);
}

#[test]
fn a_text_field_keeps_ctrl_alt_keys() {
    let mut h = harness_with(app());
    // The palette's query takes focus; AltGr (Ctrl+Alt) + 0 types a character there on many layouts.
    let spot = h.state().viewport.center();
    h.state_mut().palette = Some(Palette::new(Pos2::ZERO, spot));
    h.run();
    assert!(canvas::text_input_focused(&h.ctx));
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::Equals);
    h.run();
    assert_eq!(h.state().interface_size().percent(), 100);
}

#[test]
fn the_palette_offers_the_size_commands_that_would_do_something() {
    let mut h = harness_with(app());
    let offered = |h: &Harness<'static, NodeCraftApp>| -> Vec<String> {
        h.state()
            .palette_entries()
            .into_iter()
            // Only the entries the palette would run: since #1121 it also lists the ones that
            // can't run now, greyed with their reason.
            .filter(|entry| entry.available() && matches!(entry.target, palette::Target::Command(id) if id.starts_with("view.ui_scale.")))
            .map(|entry| entry.label)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    assert_eq!(offered(&h), ["Larger interface", "Smaller interface"]);
    h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent": 200})).unwrap();
    h.run();
    assert_eq!(offered(&h), ["Reset interface size", "Smaller interface"]);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_size_persists_across_restarts_and_a_damaged_value_falls_back_with_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[appearance]\ntheme = \"light\"\ninterface_size = \"huge\"\n").unwrap();
    let open = || nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(dir.path().to_path_buf())).0;
    let ctx = egui::Context::default();
    let app = NodeCraftApp::with_settings_and_storage(&ctx, open(), None).unwrap();
    assert_eq!(app.interface_size().percent(), 100);
    assert!(app.notice.as_deref().is_some_and(|notice| notice.contains(nodecraft_engine::settings::InterfaceSize::DAMAGED)), "{:?}", app.notice);
    assert_eq!(app.theme, ThemeKind::Light, "the rest of the section is kept");
    let mut h = harness_with(app);
    assert_eq!(h.ctx.zoom_factor(), 1.0);
    assert!(!h.state().store.as_ref().unwrap().get().appearance.interface_size.damaged(), "the damaged value is replaced");
    assert!(!h.state().interface_size().damaged(), "and the window's size is clean once saved");
    assert!(h.state_mut().session.enabled("view.ui_scale.reset", &json!({})).is_err(), "reset has nothing left to do");
    h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent": 175})).unwrap();
    h.run();
    h.state_mut().store.as_mut().unwrap().flush().unwrap();
    drop(h);
    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(text.contains("interface_size = 175"), "{text}");

    let app = NodeCraftApp::with_settings_and_storage(&egui::Context::default(), open(), None).unwrap();
    assert_eq!(app.interface_size().percent(), 175);
    assert!(!app.notice.as_deref().unwrap_or_default().contains(nodecraft_engine::settings::InterfaceSize::DAMAGED), "{:?}", app.notice);
    let h = harness_with(app);
    assert!((h.ctx.zoom_factor() - 1.75).abs() < 1e-4);
}
