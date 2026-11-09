//! Shared viewport and evidence setup for #959's modal/content layout regressions.
use super::*;
use egui_kittest::Harness;

pub(super) const CASES: [(Vec2, u16); 3] = [(Vec2::new(640.0, 400.0), 100), (Vec2::new(800.0, 520.0), 100), (Vec2::new(1280.0, 720.0), 200)];

pub(super) fn viewport(h: &mut Harness<'static, NodeCraftApp>, theme: ThemeKind, size: Vec2, percent: u16) {
    h.state_mut().theme = theme;
    crate::theme::apply(&h.ctx, theme);
    h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent":percent})).unwrap();
    // Apply zoom before setting physical size: doing both in one pass rescales the old size.
    h.run_steps(6);
    h.set_size(size);
    h.run_steps(16);
    assert!((h.ctx.content_rect().width() - size.x * 100.0 / f32::from(percent)).abs() < 1.0);
}

pub(super) fn modal(h: &Harness<'static, NodeCraftApp>, id: &str) -> Rect {
    let rect = egui::AreaState::load(&h.ctx, egui::Id::new(id)).unwrap().rect();
    assert!(h.ctx.content_rect().expand(1.0).contains_rect(rect), "{id}: {rect:?} leaves {:?}", h.ctx.content_rect());
    rect
}

pub(super) fn wheel(h: &mut Harness<'static, NodeCraftApp>, at: Pos2, delta: f32) {
    h.hover_at(at);
    h.run_steps(2);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, delta),
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(20);
}

pub(super) fn evidence(h: &mut Harness<'static, NodeCraftApp>, name: &str, theme: ThemeKind, size: Vec2, percent: u16) {
    if let Some(dir) = std::env::var_os("NODECRAFT_LAYOUT959_EVIDENCE") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(dir.join(format!("{name}-{theme:?}-{}x{}-{percent}.png", size.x, size.y))).unwrap();
    }
}
