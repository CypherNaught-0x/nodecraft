//! The eframe app: the canvas with its cards, and the overlays floating above it.

use egui::{Pos2, Rect, Sense, Vec2};
use nodecraft_engine::settings::{CanvasSettings, NodeCraftSettings, Theme};

use crate::canvas::{self, CanvasView};
use crate::card::{self, StubCard};
use crate::theme::{self, ThemeKind};

pub struct NodeCraftApp {
    pub theme: ThemeKind,
    pub view: CanvasView,
    pub cards: Vec<StubCard>,
    pub selected: Option<u64>,
    /// Canvas preferences from the settings (scroll behaviour, zoom speed).
    pub canvas_prefs: CanvasSettings,
    /// The canvas rect of the last frame, in screen points.
    pub viewport: Rect,
    /// The first frame centres the cards; later frames keep the user's view.
    centred: bool,
    /// Where settings are saved; `None` in tests and the browser build.
    #[cfg(not(target_arch = "wasm32"))]
    store: Option<nodecraft_engine::settings::SettingsStore>,
}

impl NodeCraftApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut app = Self::headless();
        app.apply_settings(&NodeCraftSettings::default());
        theme::apply(ctx, app.theme);
        // ⌘= / ⌘- / ⌘0 zoom the canvas, not the whole UI.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        app
    }

    /// The app with settings from `store`, which it saves while running and flushes on exit.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_settings(ctx: &egui::Context, store: nodecraft_engine::settings::SettingsStore) -> Self {
        let mut app = Self::headless();
        app.apply_settings(store.get());
        app.store = Some(store);
        theme::apply(ctx, app.theme);
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        app
    }

    /// Takes the values the shell uses from `settings`.
    pub fn apply_settings(&mut self, settings: &NodeCraftSettings) {
        self.theme = match settings.appearance.theme {
            Theme::Dark => ThemeKind::Dark,
            Theme::Light => ThemeKind::Light,
        };
        self.canvas_prefs = settings.canvas.clone();
    }

    /// The app state without touching an egui context (tests).
    pub fn headless() -> Self {
        let card = StubCard {
            id: 1,
            label: "Prompt".into(),
            body: "A stub node card. Drag it around; drag the canvas to pan, pinch or ⌘-scroll to zoom, ⌘0 to reset.".into(),
            rect: Rect::from_min_size(Pos2::new(-160.0, -110.0), Vec2::new(320.0, 220.0)),
        };
        Self {
            theme: ThemeKind::default(),
            view: CanvasView::default(),
            cards: vec![card],
            selected: None,
            canvas_prefs: CanvasSettings::default(),
            viewport: Rect::NOTHING,
            centred: false,
            #[cfg(not(target_arch = "wasm32"))]
            store: None,
        }
    }

    /// World point the view centres on at startup and on ⌘0: the middle of all cards.
    pub fn home(&self) -> Pos2 {
        self.cards.iter().map(|c| c.rect).reduce(|a, b| a.union(b)).map_or(Pos2::ZERO, |r| r.center())
    }

    /// The whole UI inside `ui` (the window, or a test harness).
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let t = self.theme.tokens();
        let viewport = ui.max_rect();
        self.viewport = viewport;
        if !self.centred {
            self.view.reset(viewport, self.home());
            self.centred = true;
        }
        let background = ui.interact(viewport, ui.id().with("canvas"), Sense::click_and_drag());
        let home = self.home();
        canvas::navigate(ui, &background, &mut self.view, home, &self.canvas_prefs);
        if background.clicked() {
            self.selected = None;
        }

        let painter = ui.painter_at(viewport);
        painter.rect_filled(viewport, 0.0, t.canvas_bg);
        canvas::paint_grid(&painter, viewport, &self.view, t);

        let origin = viewport.min;
        let mut pan = Vec2::ZERO;
        for c in &mut self.cards {
            let selected = self.selected == Some(c.id);
            let r = card::show(ui, origin, &self.view, c, selected, t);
            if r.clicked() || r.drag_started_by(egui::PointerButton::Primary) {
                self.selected = Some(c.id);
            }
            // Middle and right drags pan the canvas even when they start on a card.
            if r.dragged_by(egui::PointerButton::Middle) || r.dragged_by(egui::PointerButton::Secondary) {
                pan += r.drag_delta();
            }
        }
        self.view.pan_by(pan);

        self.zoom_badge(ui, viewport);
    }

    /// Bottom-right zoom percentage; clicking it resets the view.
    fn zoom_badge(&mut self, ui: &mut egui::Ui, viewport: Rect) {
        let t = self.theme.tokens();
        let frame = egui::Frame::NONE
            .fill(t.panel_fill)
            .stroke(egui::Stroke::new(1.0, t.panel_stroke))
            .corner_radius(t.panel_radius)
            .inner_margin(egui::Margin::symmetric(6, 4));
        let mut reset = false;
        egui::Area::new(ui.id().with("zoom_badge"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -16.0))
            .constrain_to(viewport)
            .show(ui.ctx(), |ui| {
                frame.show(ui, |ui| {
                    let label = egui::RichText::new(format!("{:.0}%", self.view.zoom * 100.0)).color(t.text_muted).monospace();
                    reset = ui.add(egui::Button::new(label).frame(false)).on_hover_text("Reset view (⌘0)").clicked();
                });
            });
        if reset {
            self.view.reset(viewport, self.home());
        }
    }
}

impl eframe::App for NodeCraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store {
            let tick = store.tick(std::time::Instant::now());
            if let nodecraft_engine::settings::SaveStatus::Failed(e) = &tick.status {
                log::warn!("settings not saved, will retry: {e}");
            }
            if let Some(deadline) = tick.next_deadline {
                ctx.request_repaint_after(deadline.saturating_duration_since(std::time::Instant::now()));
            }
        }
        // Last, so it sees any `CancelClose` from an unsaved-changes check (none in M0).
        craft_quit::on_close_request(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store
            && let Err(e) = store.flush()
        {
            log::warn!("settings not saved on exit: {e}");
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.theme.tokens().canvas_bg.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    fn harness() -> Harness<'static, NodeCraftApp> {
        Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless())
    }

    #[test]
    fn first_frame_centres_the_card_at_100_percent() {
        let h = harness();
        let app = h.state();
        assert_eq!(app.view.zoom, 1.0);
        let screen = app.view.rect_to_screen(app.viewport.min, app.cards[0].rect);
        assert!((screen.center() - app.viewport.center()).length() < 1.0, "{screen:?} in {:?}", app.viewport);
    }

    #[test]
    fn dragging_the_background_pans() {
        let mut h = harness();
        let pan = h.state().view.pan;
        h.drag_at(Pos2::new(50.0, 50.0));
        h.run();
        h.hover_at(Pos2::new(150.0, 90.0));
        h.run();
        h.drop_at(Pos2::new(150.0, 90.0));
        h.run();
        let moved = h.state().view.pan - pan;
        assert!((moved - Vec2::new(100.0, 40.0)).length() < 1.0, "{moved:?}");
        assert_eq!(h.state().cards[0].rect, NodeCraftApp::headless().cards[0].rect);
    }

    #[test]
    fn dragging_a_card_moves_and_selects_it() {
        let mut h = harness();
        let before = h.state().cards[0].rect;
        h.drag_at(Pos2::new(500.0, 350.0));
        h.run();
        h.hover_at(Pos2::new(560.0, 330.0));
        h.run();
        h.drop_at(Pos2::new(560.0, 330.0));
        h.run();
        let app = h.state();
        assert_eq!(app.selected, Some(1));
        let moved = app.cards[0].rect.min - before.min;
        assert!((moved - Vec2::new(60.0, -20.0)).length() < 1.0, "{moved:?}");
    }

    #[test]
    fn right_drag_on_a_card_pans_instead_of_moving_it() {
        let mut h = harness();
        let pan = h.state().view.pan;
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        h.hover_at(Pos2::new(500.0, 350.0));
        h.event(button(Pos2::new(500.0, 350.0), true));
        h.run();
        h.hover_at(Pos2::new(530.0, 370.0));
        h.run();
        h.event(button(Pos2::new(530.0, 370.0), false));
        h.run();
        let app = h.state();
        assert_eq!(app.cards[0].rect, NodeCraftApp::headless().cards[0].rect);
        assert!((app.view.pan - pan - Vec2::new(30.0, 20.0)).length() < 1.0, "{:?}", app.view.pan - pan);
        assert_eq!(app.selected, None);
    }

    #[test]
    fn command_zero_resets_after_zooming() {
        let mut h = harness();
        h.hover_at(Pos2::new(200.0, 200.0));
        h.event(egui::Event::Zoom(2.0));
        h.run();
        assert!((h.state().view.zoom - 2.0).abs() < 1e-3);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
        h.run();
        assert_eq!(h.state().view.zoom, 1.0);
    }
}
