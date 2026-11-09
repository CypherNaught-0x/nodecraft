//! First run and the empty canvas (#1119): say how to add a node and how to set up a provider.
//!
//! All of it is view state, drawn by the shell only, so programmatic sessions (CLI, MCP, the
//! control channel) never see it, and nothing here touches the document.
//! - An empty canvas shows a quiet panel in its middle: double-click to add a node (with a button
//!   that opens the quick-add palette there), drop media, or open a Technique.
//! - While the canvas holds only the sample Text card with nothing wired, a one-time callout beside
//!   it says to drag from its + to connect a model; dismissing it, or wiring anything, ends it.
//! - With no API key and no ComfyUI server, a generation card's submit tooltip and the run quote
//!   name the missing setup ([`NodeCraftApp::providers_ready`]).
use super::*;

/// Where the "nobody has set up a provider" flag is published for the cards each frame.
pub(crate) fn providers_ready_id() -> egui::Id {
    egui::Id::new("nodecraft.providers_ready")
}

/// Whether a person dismissed the sample card's callout in this window.
fn callout_dismissed_id() -> egui::Id {
    egui::Id::new("nodecraft.sample_callout_dismissed")
}

/// What a generation card says when no provider is set up yet (#1119).
pub(crate) const NO_PROVIDER: &str = "No provider is set up yet: add an API key, or a ComfyUI server, before generating";

impl NodeCraftApp {
    /// Is any way to generate set up: a stored API key account, a ComfyUI server, a LiteLLM proxy
    /// or a provider's own base URL (a local server needs no key)?
    pub(super) fn providers_ready(&self) -> bool {
        if !self.key_accounts.accounts().is_empty() {
            return true;
        }
        let settings = self.provider_settings();
        !settings.comfy_servers().is_empty() || settings.litellm_url.as_deref().is_some_and(|url| !url.trim().is_empty()) || !settings.base_urls.is_empty()
    }

    /// [`Self::providers_ready`], looked up at most once a second: settings are cloned to answer it.
    fn providers_ready_cached(&self, ctx: &egui::Context) -> bool {
        let now = ctx.input(|input| input.time);
        let cache = egui::Id::new("nodecraft.providers_ready_at");
        if let Some((at, ready)) = ctx.data(|data| data.get_temp::<(f64, bool)>(cache))
            && (0.0..1.0).contains(&(now - at))
        {
            return ready;
        }
        let ready = self.providers_ready();
        ctx.data_mut(|data| data.insert_temp(cache, (now, ready)));
        ready
    }

    /// Can this window add nodes? A shared-room viewer who isn't editing gets no prompts to.
    pub(super) fn can_add_nodes(&self) -> bool {
        let probe = json!({"canvas": self.canvas, "id": NodeId::from_bytes([0; 16]), "kind": "source.text", "pos": {"x": 0.0, "y": 0.0}});
        !matches!(self.session.enabled("node.create", &probe), Err(error) if error.kind == ErrorKind::Forbidden)
    }

    /// One frame of the first-run guidance: the empty-canvas hint or the sample card's callout,
    /// and the providers flag the cards read.
    pub(super) fn empty_canvas(&mut self, ui: &egui::Ui, viewport: Rect, blocked: bool, t: &Tokens) {
        // Clear an earlier editor frame when the window switches to view-only access.
        // Programmatic windows and viewers get no setup guidance, including card tooltips.
        if !self.guidance || !self.can_add_nodes() {
            ui.ctx().data_mut(|data| data.remove::<bool>(providers_ready_id()));
            return;
        }
        let ready = self.providers_ready_cached(ui.ctx());
        ui.ctx().data_mut(|data| data.insert_temp(providers_ready_id(), ready));
        self.add_node_button(ui.ctx(), viewport, blocked, t);
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        if canvas.nodes().is_empty() {
            self.empty_hint(ui.ctx(), viewport, blocked, t);
            return;
        }
        let sample = canvas.edges().is_empty() && canvas.nodes().len() == 1;
        let node = canvas.nodes().values().next().filter(|node| sample && node.kind == "source.text").map(|node| node.id);
        let dismissed = ui.ctx().data(|data| data.get_temp::<bool>(callout_dismissed_id())).unwrap_or(false);
        if let Some(node) = node
            && !dismissed
            && let Some(card) = self.painted.get(&node)
        {
            self.sample_callout(ui.ctx(), viewport, *card, t);
        }
    }

    /// Opens the quick-add palette in the middle of the canvas.
    fn open_palette_in_middle(&mut self, viewport: Rect) {
        let at = viewport.center();
        self.palette = Some(Palette::new(self.view.to_world(viewport.min, at), at));
        self.turn_into = None;
    }

    /// The always-there way to add a node without double-clicking (#1119): a floating button in
    /// the canvas's lower-left corner that opens the quick-add palette in the middle.
    fn add_node_button(&mut self, ctx: &egui::Context, viewport: Rect, blocked: bool, t: &Tokens) {
        let mut add = false;
        egui::Area::new(egui::Id::new("nodecraft.add_node"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(viewport.left_bottom() + Vec2::new(t.card_padding, -t.card_padding))
            .constrain_to(viewport)
            .interactable(!blocked)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.card_padding * 0.5)
                    .show(ui, |ui| {
                        add = ui
                            .button("+ Add node")
                            .on_hover_text("Open the quick-add palette in the middle of the canvas (or double-click the canvas)")
                            .clicked();
                    });
            });
        if add {
            self.open_palette_in_middle(viewport);
        }
    }

    /// The quiet panel in the middle of an empty canvas.
    fn empty_hint(&mut self, ctx: &egui::Context, viewport: Rect, blocked: bool, t: &Tokens) {
        let mut add = false;
        let mut techniques = false;
        egui::Area::new(egui::Id::new("nodecraft.empty_canvas"))
            .order(egui::Order::Background)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(viewport.center())
            .constrain_to(viewport)
            .interactable(!blocked)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.card_padding)
                    .show(ui, |ui| {
                        ui.set_max_width(t.dialog_width.min(viewport.width() - t.card_padding * 4.0).max(t.field_min_width));
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("This canvas is empty").color(t.text).strong());
                            ui.label(egui::RichText::new("Double-click anywhere to add a node").color(t.text_muted));
                            add = ui.button("Add a node").on_hover_text("Open the quick-add palette in the middle of the canvas").clicked();
                            ui.label(egui::RichText::new("Drop images, video or a folder here").color(t.text_muted));
                            techniques = ui.button("Open a Technique…").on_hover_text("Place a saved Technique or a starter template").clicked();
                        });
                    });
            });
        if add {
            self.open_palette_in_middle(viewport);
        }
        if techniques && !self.library.open {
            self.toggle_techniques();
        }
    }

    /// The one-time callout beside the sample card.
    fn sample_callout(&mut self, ctx: &egui::Context, viewport: Rect, card: Rect, t: &Tokens) {
        let card = self.view.rect_to_screen(viewport.min, card);
        if !card.is_finite() || !viewport.intersects(card) {
            return;
        }
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("nodecraft.sample_callout"))
            .order(egui::Order::Background)
            .pivot(egui::Align2::LEFT_CENTER)
            .fixed_pos(card.right_center() + Vec2::new(t.handle_radius * 2.0 + t.control_gap * 2.0, 0.0))
            .constrain_to(viewport)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.accent))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.control_gap * 2.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Drag from + to connect to a model").color(t.text));
                            dismiss = crate::history::named(ui.small_button("×"), "Dismiss the tip").on_hover_text("Don't show this tip again").clicked();
                        });
                    });
            });
        if dismiss {
            ctx.data_mut(|data| data.insert_temp(callout_dismissed_id(), true));
        }
    }
}
