//! One ordered set of shell controls, shared by the wide row and its overflow menu (#956).
use super::*;
use crate::icons::Icon;

#[derive(Default)]
pub(super) struct Topbar {
    pub rect: Option<Rect>,
    pub search: Option<Rect>,
    pub comments: Option<Rect>,
}

pub(super) fn popup_id() -> egui::Id {
    egui::Id::new("nodecraft.topbar.more")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Sheets,
    Techniques,
    Elements,
    OpenApp,
    Queue,
    Share,
    Budgets,
    Keys,
    #[cfg(not(target_arch = "wasm32"))]
    Comfy,
    Providers,
    Plugins,
    Search,
    Comments,
}

impl Entry {
    pub(super) fn icon(self) -> Icon {
        match self {
            Self::Sheets => Icon::Table,
            Self::Techniques => Icon::Technique,
            Self::Elements => Icon::Elements,
            Self::OpenApp => Icon::Apps,
            Self::Queue => Icon::Queue,
            Self::Share => Icon::Share,
            Self::Budgets => Icon::Wallet,
            Self::Keys => Icon::Key,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Comfy => Icon::Server,
            Self::Providers => Icon::Cloud,
            Self::Plugins => Icon::Plugin,
            Self::Search => Icon::Search,
            Self::Comments => Icon::Comments,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Sheets => "Sheets",
            Self::Techniques => "Techniques",
            Self::Elements => "Elements",
            Self::OpenApp => "Open as app",
            Self::Queue => "Queue",
            Self::Share => "Share",
            Self::Budgets => "Budgets",
            Self::Keys => "API keys",
            #[cfg(not(target_arch = "wasm32"))]
            Self::Comfy => "ComfyUI",
            Self::Providers => "Providers",
            Self::Plugins => "Plugins",
            Self::Search => "Search",
            Self::Comments => "Comments",
        }
    }

    fn group(self) -> u8 {
        match self {
            Self::Sheets | Self::Techniques | Self::Elements | Self::OpenApp => 0,
            Self::Queue | Self::Share | Self::Budgets => 1,
            Self::Keys | Self::Providers | Self::Plugins => 2,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Comfy => 2,
            Self::Search | Self::Comments => 3,
        }
    }
}

struct Control {
    entry: Entry,
    enabled: bool,
    active: bool,
    badge: Option<String>,
    hint: String,
}

impl Control {
    fn width(&self, ui: &egui::Ui, t: &Tokens) -> f32 {
        let text_width = |text: &str| ui.painter().layout_no_wrap(text.to_owned(), egui::TextStyle::Button.resolve(ui.style()), t.text).size().x;
        text_width(self.entry.label())
            + t.icon_size.min(egui::TextStyle::Button.resolve(ui.style()).size)
            + t.control_gap
            + t.navigation_gap * 2.0
            + self.badge.as_ref().map_or(0.0, |badge| text_width(badge) + t.control_gap)
    }

    fn show(&self, ui: &mut egui::Ui, t: &Tokens) -> egui::Response {
        let color = if self.active { t.accent } else { t.text_muted };
        let mut button = self
            .entry
            .icon()
            .button(ui, egui::RichText::new(self.entry.label()).color(color), t)
            .gap(t.control_gap)
            .wrap_mode(egui::TextWrapMode::Extend)
            .frame_when_inactive(false)
            .selected(self.active)
            .image_tint_follows_text_color(true)
            .min_size(Vec2::new(0.0, t.navigation_toolbar_height));
        if let Some(badge) = &self.badge {
            button = button.right_text(egui::RichText::new(badge).color(color));
        }
        let response = ui.add_enabled(self.enabled, button);
        // Keep the run count in the established accessible name as well as the visible badge.
        let label = if self.entry == Entry::Search {
            if cfg!(target_os = "macos") { "Search (⌘F)" } else { "Search (Ctrl+F)" }.to_owned()
        } else if self.entry == Entry::Queue {
            self.badge.as_ref().map_or_else(|| self.entry.label().to_owned(), |badge| format!("Queue ({badge})"))
        } else {
            self.entry.label().to_owned()
        };
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, self.enabled, self.active, label.as_str()));
        response.on_hover_text(&self.hint)
    }
}

impl NodeCraftApp {
    pub(super) fn topbar_rect(&self) -> Rect {
        self.topbar.rect.unwrap_or_else(|| {
            let t = self.theme.tokens();
            Rect::from_min_size(
                self.viewport.right_top() + Vec2::new(-t.navigation_margin - t.search_button_width, t.navigation_margin),
                Vec2::new(t.search_button_width, t.navigation_toolbar_height),
            )
        })
    }

    fn topbar_controls(&mut self, ctx: &egui::Context) -> Vec<Control> {
        use Entry::*;
        let sheet = self.on_sheet();
        let search = self.search_context_scoped(ctx, Some(popup_id()));
        let comments = self.discussion_context(ctx, Some(popup_id()));
        let queue = self.queue_label();
        let queue_badge = queue.as_deref().and_then(|label| label.strip_prefix("Queue (")).and_then(|label| label.strip_suffix(')')).map(str::to_owned);
        let shell_enabled = self.palette.is_none()
            && self.turn_into.is_none()
            && self.model_choice.is_none()
            && !self.model_frame_owned
            && (!egui::Popup::is_any_open(ctx) || egui::Popup::is_id_open(ctx, popup_id()));
        let mut entries = vec![
            (Sheets, true, self.sheet.open, None, "Switch canvas, or make a batch Generate sheet".to_owned()),
            (Techniques, true, self.library.open, None, "Place a published technique or keep one in the library".to_owned()),
            (
                Elements,
                self.session.enabled("library.elements", &json!({})).is_ok(),
                self.element_library.open,
                None,
                "Place an Element saved in the library".to_owned(),
            ),
        ];
        if self.selected_technique().is_some() {
            entries.push((OpenApp, true, self.app_mode.is_some(), None, "Fill the selected technique's inputs and run it from a simple form".to_owned()));
        }
        entries.extend([
            (Queue, queue.is_some(), self.queue_open, queue_badge, "Every job: status, provider, cost, cancel and retry".to_owned()),
            (
                Share,
                self.sharing.is_some(),
                self.sharing.as_ref().is_some_and(|sharing| sharing.panel.open),
                None,
                self.sharing.as_ref().map_or_else(|| "Live sharing is unavailable in this host".to_owned(), |sharing| sharing.chip()),
            ),
            (Budgets, true, self.budgets.open, None, "The most runs may spend per call, per session and in total".to_owned()),
            (Keys, true, self.keys.open, None, "Add, check and remove provider keys".to_owned()),
        ]);
        #[cfg(not(target_arch = "wasm32"))]
        entries.push((Comfy, self.comfy_nodes.is_some(), self.comfy_server.open, None, "Set up ComfyUI servers, check them and read their nodes".to_owned()));
        if let Some(providers) = &self.browser_providers {
            entries.push((Providers, true, providers.open, None, "ComfyUI and LiteLLM server addresses for this site".to_owned()));
        }
        entries.extend([
            (
                Plugins,
                self.session.enabled("plugins.list", &json!({})).is_ok(),
                self.plugin_panel.open,
                None,
                "Install, enable and grant plugin packages".to_owned(),
            ),
            (
                Search,
                !sheet && search.viewport_available && !search.blocked,
                self.search.open,
                None,
                "Search this canvas; finish any active editor or gesture first".to_owned(),
            ),
            (Comments, !sheet && !comments.blocked, self.comments.open, None, "Discussions pinned to this canvas".to_owned()),
        ]);
        entries.into_iter().map(|(entry, enabled, active, badge, hint)| Control { entry, enabled: enabled && shell_enabled, active, badge, hint }).collect()
    }

    /// Returns foreground pointer ownership, including the overflow menu's opening frame.
    pub(super) fn topbar(&mut self, ui: &mut egui::Ui) -> bool {
        let t = self.theme.tokens();
        let controls = self.topbar_controls(ui.ctx());
        // The independent tag-filter control keeps a reserved slot on the left. Its
        // panel stays below this row; it never overlaps a menu or a discussion editor.
        let format = if self.selection.len() >= 2 { self.layout_toolbar_width() + t.navigation_gap } else { 0.0 };
        let available = (self.viewport.width() - t.navigation_margin * 2.0 - self.closed_tag_width() - t.navigation_gap - format).max(1.0);
        let group_count = controls.windows(2).filter(|pair| pair.first().zip(pair.get(1)).is_some_and(|(a, b)| a.entry.group() != b.entry.group())).count();
        let widths: Vec<f32> = controls.iter().map(|control| control.width(ui, t)).collect();
        let needed = widths.iter().sum::<f32>() + t.navigation_gap * (controls.len().saturating_sub(1) as f32 + group_count as f32 * 2.0);
        let collapsed = needed > available - t.navigation_gap * 2.0;
        if !collapsed {
            egui::Popup::close_id(ui.ctx(), popup_id());
        }
        let menu_enabled = controls.iter().any(|control| control.enabled)
            && !self.text_focused
            && !canvas::text_input_focused(ui.ctx())
            && self.drag.is_none()
            && self.wiring.is_none()
            && self.marquee.is_none()
            && self.comments.drag.is_none()
            && !self.comments.gesture_cancelled
            && !self.navigation.pointer_owned
            && self.navigation.drag.is_none();
        let mut action = None;
        let mut menu_rect = None;
        self.topbar.search = None;
        self.topbar.comments = None;
        let toolbar = egui::Area::new(egui::Id::new("comments_toolbar"))
            .order(egui::Order::Foreground)
            .movable(false)
            .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-t.navigation_margin, t.navigation_margin))
            .constrain_to(self.viewport)
            .show(ui.ctx(), |ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(t.navigation_gap);
                ui.spacing_mut().button_padding = Vec2::new(t.navigation_gap, t.control_vertical_padding);
                ui.visuals_mut().selection.bg_fill = t.panel_stroke;
                ui.visuals_mut().selection.stroke = egui::Stroke::new(t.handle_stroke, t.accent);
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.navigation_gap)
                    .show(ui, |ui| {
                        if collapsed {
                            let response = ui
                                .add_enabled(
                                    menu_enabled,
                                    Icon::More.button(ui, "More", t).gap(t.control_gap).min_size(Vec2::new(0.0, t.navigation_toolbar_height)),
                                )
                                .on_hover_text("All menu entries; finish any active editor or gesture before opening");
                            if let Some(menu) = egui::Popup::menu(&response).id(popup_id()).show(|ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(
                                        (self.viewport.height() - t.navigation_toolbar_height - t.navigation_margin * 3.0).max(t.navigation_toolbar_height),
                                    )
                                    .show(ui, |ui| {
                                        let mut group = None;
                                        for control in &controls {
                                            if group.is_some_and(|group| group != control.entry.group()) {
                                                ui.separator();
                                            }
                                            group = Some(control.entry.group());
                                            if control.show(ui, t).clicked() {
                                                action = Some(control.entry);
                                                ui.close();
                                            }
                                        }
                                    });
                            }) {
                                menu_rect = Some(menu.response.rect);
                            }
                        } else {
                            ui.set_width((available - t.navigation_gap * 2.0).max(1.0));
                            ui.horizontal(|ui| {
                                let mut group = None;
                                for control in &controls {
                                    if group.is_some_and(|group| group != control.entry.group()) {
                                        if control.entry.group() == 3 {
                                            let tail = controls.iter().filter(|c| c.entry.group() == 3).map(|c| c.width(ui, t) + t.navigation_gap).sum::<f32>();
                                            ui.add_space((ui.available_width() - tail - t.navigation_gap * 2.0).max(0.0));
                                        }
                                        ui.separator();
                                    }
                                    group = Some(control.entry.group());
                                    let response = control.show(ui, t);
                                    match control.entry {
                                        Entry::Search => self.topbar.search = Some(response.rect),
                                        Entry::Comments => self.topbar.comments = Some(response.rect),
                                        _ => {}
                                    }
                                    if response.clicked() {
                                        action = Some(control.entry);
                                    }
                                }
                            });
                        }
                    });
            });
        self.topbar.rect = Some(toolbar.response.rect);
        let exposed = ui.ctx().pointer_interact_pos().is_some_and(|pos| {
            toolbar.response.rect.contains(pos) && ui.ctx().layer_id_at(pos) == Some(toolbar.response.layer_id)
                || menu_rect.is_some_and(|rect| rect.contains(pos))
        });
        if let Some(action) = action {
            egui::Popup::close_id(ui.ctx(), popup_id());
            self.activate_topbar(action, ui.ctx());
        }
        exposed
    }

    fn activate_topbar(&mut self, entry: Entry, ctx: &egui::Context) {
        match entry {
            Entry::Sheets => self.sheet.open = !self.sheet.open,
            Entry::Techniques => self.toggle_techniques(),
            Entry::Elements => self.toggle_elements(),
            Entry::OpenApp => {
                if let Some(node) = self.selected_technique() {
                    self.open_app_mode(node);
                }
            }
            Entry::Queue => self.queue_open = !self.queue_open,
            Entry::Share => {
                if let Some(sharing) = &mut self.sharing {
                    sharing.panel.open = !sharing.panel.open;
                }
            }
            Entry::Budgets => {
                if self.budgets.open {
                    self.budgets.open = false;
                } else {
                    self.budgets.open();
                }
            }
            Entry::Keys => self.keys.open = !self.keys.open,
            #[cfg(not(target_arch = "wasm32"))]
            Entry::Comfy => self.comfy_server.open = !self.comfy_server.open,
            Entry::Providers => {
                if let Some(providers) = &mut self.browser_providers {
                    providers.open = !providers.open;
                }
            }
            Entry::Plugins => self.plugin_panel.open = !self.plugin_panel.open,
            Entry::Search => {
                self.search_focus_pending = true;
                if let Err(error) = self.dispatch_search(ctx, "canvas.search_open", &json!({})) {
                    self.search_focus_pending = false;
                    self.notice = Some(error.message);
                }
            }
            Entry::Comments => {
                if let Err(error) = self.dispatch_comments(ctx, "canvas.comments_toggle", &json!({})) {
                    self.notice = Some(error.message);
                }
            }
        }
        ctx.request_repaint();
    }
}
