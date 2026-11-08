//! One ordered set of shell controls, shared by the wide row and its overflow menu (#956).
use super::*;
use crate::icons::Icon;

#[derive(Default)]
pub(super) struct Topbar {
    pub rect: Option<Rect>,
    pub search: Option<Rect>,
    pub comments: Option<Rect>,
    /// The More button, while some entries have no room in the row.
    pub more: Option<Rect>,
    /// What the File menu opens below (#1123): the File entry, or More while File is in it.
    pub file: Option<Rect>,
}

pub(super) fn popup_id() -> egui::Id {
    egui::Id::new("nodecraft.topbar.more")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    File,
    Tags,
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
    Automation,
    Plugins,
    Search,
    Comments,
}

impl Entry {
    pub(super) fn icon(self) -> Icon {
        match self {
            Self::File => Icon::File,
            Self::Tags => Icon::Tag,
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
            Self::Automation => Icon::Plug,
            Self::Plugins => Icon::Plugin,
            Self::Search => Icon::Search,
            Self::Comments => Icon::Comments,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Tags => "Tag filter",
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
            Self::Automation => "Automation",
            Self::Plugins => "Plugins",
            Self::Search => "Search",
            Self::Comments => "Comments",
        }
    }

    fn group(self) -> u8 {
        match self {
            Self::File | Self::Tags => LEAD,
            Self::Sheets | Self::Techniques | Self::Elements | Self::OpenApp => 0,
            Self::Queue | Self::Share | Self::Budgets => 1,
            Self::Keys | Self::Providers | Self::Automation | Self::Plugins => 2,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Comfy => 2,
            Self::Search | Self::Comments => TAIL,
        }
    }

    /// When the entry moves into More (#1115, #1123): lower ranks go first, and within a rank
    /// the rightmost entry goes first. The navigation, activity and service entries go first,
    /// an open panel's entry next, then Tag filter, then Comments and Search, and File last:
    /// it is the only way to open and save.
    pub(super) fn rank(self, active: bool) -> u8 {
        match self {
            Self::File => 4,
            Self::Search | Self::Comments => 3,
            Self::Tags => 2,
            _ if active => 1,
            _ => 0,
        }
    }
}

/// Which entries the row shows (#1115); the rest go into More. `widths` are the entries' own
/// widths, `groups` their dividers' groups, `ranks` when each leaves ([`Entry::rank`]: lowest
/// first, from the right within a rank), `more` the More button's width and `gap` the row's
/// spacing. The row never needs more than `available`.
pub(super) fn fit(widths: &[f32], groups: &[u8], ranks: &[u8], more: f32, available: f32, gap: f32) -> Vec<bool> {
    let tail = |i: usize| groups.get(i).is_some_and(|g| *g == TAIL);
    let needed = |shown: &[bool]| {
        let mut total = 0.0;
        let mut count = 0usize;
        let mut last_group = None;
        let mut hidden = false;
        for (i, show) in shown.iter().enumerate() {
            if !show {
                hidden = true;
                continue;
            }
            let group = groups.get(i).copied();
            if last_group.is_some() && last_group != group {
                total += gap * 2.0;
            }
            last_group = group;
            total += widths.get(i).copied().unwrap_or(0.0);
            count += 1;
        }
        if hidden {
            total += more;
            count += 1;
            // More sits before the tail: a divider whenever a tail entry still shows.
            if shown.iter().enumerate().any(|(i, s)| *s && tail(i)) && !shown.iter().enumerate().any(|(i, s)| *s && !tail(i)) {
                total += gap * 2.0;
            }
        }
        total + gap * count.saturating_sub(1) as f32
    };
    let mut shown = vec![true; widths.len()];
    // The order entries leave the row in: by rank, and from the right within a rank.
    let mut order: Vec<usize> = (0..widths.len()).collect();
    order.sort_by_key(|i| (ranks.get(*i).copied().unwrap_or(0), std::cmp::Reverse(*i)));
    for i in order {
        if needed(&shown) <= available {
            break;
        }
        if let Some(show) = shown.get_mut(i) {
            *show = false;
        }
    }
    shown
}

/// The group of Search and Comments, kept at the row's right end.
const TAIL: u8 = 3;
/// The group of File and Tag filter, at the row's left end (#1123).
const LEAD: u8 = 4;

struct Control {
    entry: Entry,
    enabled: bool,
    active: bool,
    badge: Option<String>,
    hint: String,
}

/// The File menu's popup id (#1123).
pub(super) fn file_popup_id() -> egui::Id {
    egui::Id::new("nodecraft.topbar.file")
}

/// The File menu (#1123): a menu opening below `anchor` (the File entry, or More while File is
/// in it), on the screen and scrolling when the window is short. `open` is its open state;
/// clicking outside it or Escape clears it. Entries close it by clearing `open` themselves.
pub(super) fn file_popup<R>(ctx: &egui::Context, anchor: Rect, open: &mut bool, t: &Tokens, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    if !*open || !anchor.is_finite() {
        return None;
    }
    let screen = ctx.content_rect();
    let room = (screen.bottom() - anchor.bottom() - t.navigation_margin).max(t.navigation_toolbar_height);
    let width = t.file_menu_width.min((screen.width() - t.navigation_margin * 2.0).max(t.navigation_toolbar_height));
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("comments_toolbar"));
    egui::Popup::new(file_popup_id(), ctx.clone(), anchor, layer)
        .kind(egui::PopupKind::Menu)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .style(egui::containers::menu::menu_style)
        .gap(t.navigation_gap)
        .width(width)
        .open_bool(open)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            egui::ScrollArea::vertical().max_height(room).show(ui, add).inner
        })
        .map(|shown| shown.inner)
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
        let color = if self.active { t.selection_text } else { t.text_muted };
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
            crate::shortcut::with_shortcut(ui.ctx(), "Search", "Command+F")
        } else if self.entry == Entry::Queue {
            self.badge.as_ref().map_or_else(|| self.entry.label().to_owned(), |badge| format!("Queue ({badge})"))
        } else if self.entry == Entry::Tags {
            self.badge.as_ref().map_or_else(|| self.entry.label().to_owned(), |badge| format!("Tag filter ({badge} matches)"))
        } else {
            self.entry.label().to_owned()
        };
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, self.enabled, self.active, label.as_str()));
        // The hint explains the entry whether or not it can open now (#1113).
        response.on_hover_text(&self.hint).on_disabled_hover_text(&self.hint)
    }
}

/// One File menu entry: its icon and label, its shortcut on the right as this platform spells
/// it (and in its accessible name), disabled with `blocked` as the reason when project storage
/// is unavailable.
pub(super) fn menu_item(ui: &mut egui::Ui, icon: crate::icons::Icon, label: &str, key: Option<&str>, blocked: Option<&str>, t: &Tokens) -> egui::Response {
    let mut button = icon.button(ui, label, t);
    if let Some(key) = key {
        button = button.shortcut_text(crate::palette::shortcut_chip(ui.ctx(), key, false, t));
    }
    let enabled = blocked.is_none();
    let response = ui.add_enabled(enabled, button);
    let name = key.map_or_else(|| label.to_owned(), |key| crate::shortcut::with_shortcut(ui.ctx(), label, key));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name.as_str()));
    match blocked {
        Some(why) => response.on_disabled_hover_text(why),
        None => response,
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
        #[cfg(not(target_arch = "wasm32"))]
        let (file_enabled, file_open) =
            (self.recovery.is_none() && self.files.idle() && self.model_choice.is_none() && !self.model_frame_owned, self.files.menu_open());
        #[cfg(target_arch = "wasm32")]
        let (file_enabled, file_open) = (!self.web_files.blocking() && self.model_choice.is_none() && !self.model_frame_owned, self.web_files.menu_open);
        let tags = self.tag_context_scoped(ctx, Some(popup_id()));
        let tag_badge = tags.active.then(|| tags.matches.to_string());
        let mut entries = vec![
            (File, file_enabled, file_open, None, "Open, save and import projects; finish a save or open first".to_owned()),
            (
                Tags,
                !sheet && tags.viewport_available && !tags.blocked,
                tags.open,
                tag_badge,
                "Highlight the cards with chosen tags on this canvas; finish any active editor or gesture first".to_owned(),
            ),
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
        if self.automation.installed() {
            entries.push((Automation, true, self.automation.open, None, "Let AI agents on this computer connect over MCP".to_owned()));
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
        // Format selection keeps a slot on the left while several cards are selected; the Tags
        // panel opens below the row.
        let format = if self.selection.len() >= 2 { self.layout_toolbar_width() + t.navigation_gap } else { 0.0 };
        let available = (self.viewport.width() - t.navigation_margin * 2.0 - format).max(1.0);
        let widths: Vec<f32> = controls.iter().map(|control| control.width(ui, t)).collect();
        let groups: Vec<u8> = controls.iter().map(|control| control.entry.group()).collect();
        let ranks: Vec<u8> = controls.iter().map(|control| control.entry.rank(control.active)).collect();
        let more_width = {
            let font = egui::TextStyle::Button.resolve(ui.style());
            ui.painter().layout_no_wrap("More".to_owned(), font.clone(), t.text).size().x + t.icon_size.min(font.size) + t.control_gap + t.navigation_gap * 2.0
        };
        let shown = fit(&widths, &groups, &ranks, more_width, available - t.navigation_gap * 2.0, t.navigation_gap);
        let collapsed = shown.iter().any(|show| !show);
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
        let mut file_rect = None;
        let mut disabled = Vec::new();
        let mut more_rect = None;
        let toolbar = egui::Area::new(egui::Id::new("comments_toolbar"))
            .order(egui::Order::Foreground)
            .movable(false)
            .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-t.navigation_margin, t.navigation_margin))
            .constrain_to(self.viewport)
            .show(ui.ctx(), |ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(t.navigation_gap);
                ui.spacing_mut().button_padding = Vec2::new(t.navigation_gap, t.control_vertical_padding);
                ui.visuals_mut().selection.bg_fill = t.selection_fill;
                ui.visuals_mut().selection.stroke = egui::Stroke::new(t.handle_stroke, t.selection_text);
                egui::Frame::NONE
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.navigation_gap)
                    .show(ui, |ui| {
                        // With only More and the tail left the row stays compact at the right.
                        let full = shown.iter().zip(&groups).any(|(show, group)| *show && *group != TAIL);
                        if full {
                            ui.set_width((available - t.navigation_gap * 2.0).max(1.0));
                        }
                        ui.horizontal(|ui| {
                            let mut group = None;
                            let mut more_drawn = false;
                            for (control, show) in controls.iter().zip(shown.iter().copied()) {
                                if !show {
                                    continue;
                                }
                                if control.entry.group() == TAIL && collapsed && !more_drawn {
                                    more_drawn = true;
                                    (more_rect, menu_rect) = self.topbar_more(ui, &controls, &shown, menu_enabled, &mut action);
                                    group = Some(u8::MAX);
                                }
                                if group.is_some_and(|group| group != control.entry.group()) {
                                    if full && control.entry.group() == TAIL {
                                        let tail = controls
                                            .iter()
                                            .zip(widths.iter().zip(shown.iter()))
                                            .filter(|(c, (_, show))| c.entry.group() == TAIL && **show)
                                            .map(|(_, (width, _))| width + t.navigation_gap)
                                            .sum::<f32>();
                                        ui.add_space((ui.available_width() - tail - t.navigation_gap * 2.0).max(0.0));
                                    }
                                    ui.separator();
                                }
                                group = Some(control.entry.group());
                                let response = control.show(ui, t);
                                if !control.enabled {
                                    disabled.push(response.rect);
                                }
                                match control.entry {
                                    Entry::Search => self.topbar.search = Some(response.rect),
                                    Entry::Comments => self.topbar.comments = Some(response.rect),
                                    Entry::File => file_rect = Some(response.rect),
                                    _ => {}
                                }
                                if response.clicked() {
                                    action = Some(control.entry);
                                }
                            }
                            if collapsed && !more_drawn {
                                (more_rect, menu_rect) = self.topbar_more(ui, &controls, &shown, menu_enabled, &mut action);
                            }
                        });
                    });
            });
        self.topbar.rect = Some(toolbar.response.rect);
        self.topbar.more = more_rect;
        self.topbar.file = file_rect.or(more_rect);
        let exposed = ui.ctx().pointer_interact_pos().is_some_and(|pos| {
            toolbar.response.rect.contains(pos) && ui.ctx().layer_id_at(pos) == Some(toolbar.response.layer_id)
                || menu_rect.is_some_and(|rect| rect.contains(pos))
        });
        // Pressing a disabled entry (one waiting for a text editor to finish) leaves the editor
        // its keyboard focus rather than blurring it.
        if self.text_focused
            && !egui::Popup::is_any_open(ui.ctx())
            && let Some(owner) = self.last_text_focus
            && ui.ctx().memory(|m| m.focused()).is_none_or(|id| id == owner)
            && ui.input(|i| i.pointer.any_pressed() || i.pointer.any_released())
            && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            && disabled.iter().any(|rect: &Rect| rect.contains(pointer))
            && ui.ctx().layer_id_at(pointer) == Some(toolbar.response.layer_id)
        {
            ui.ctx().memory_mut(|m| m.request_focus(owner));
            ui.ctx().options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        if let Some(action) = action {
            egui::Popup::close_id(ui.ctx(), popup_id());
            self.activate_topbar(action, ui.ctx());
        }
        exposed
    }

    /// The More button and its menu, holding the entries the row has no room for, in row order.
    /// Returns the button's rect and the open menu's.
    fn topbar_more(&self, ui: &mut egui::Ui, controls: &[Control], shown: &[bool], enabled: bool, action: &mut Option<Entry>) -> (Option<Rect>, Option<Rect>) {
        let t = self.theme.tokens();
        let response = ui
            .add_enabled(enabled, Icon::More.button(ui, "More", t).gap(t.control_gap).min_size(Vec2::new(0.0, t.navigation_toolbar_height)))
            .on_hover_text("The menu entries without room in this row; finish any active editor or gesture before opening")
            .on_disabled_hover_text("Finish the active editor or gesture first");
        let menu = egui::Popup::menu(&response).id(popup_id()).show(|ui| {
            egui::ScrollArea::vertical()
                .max_height((self.viewport.height() - t.navigation_toolbar_height - t.navigation_margin * 3.0).max(t.navigation_toolbar_height))
                .show(ui, |ui| {
                    let mut group = None;
                    for (control, _) in controls.iter().zip(shown.iter()).filter(|(_, show)| !**show) {
                        if group.is_some_and(|group| group != control.entry.group()) {
                            ui.separator();
                        }
                        group = Some(control.entry.group());
                        if control.show(ui, t).clicked() {
                            *action = Some(control.entry);
                            ui.close();
                        }
                    }
                });
        });
        (Some(response.rect), menu.map(|menu| menu.response.rect))
    }

    fn activate_topbar(&mut self, entry: Entry, ctx: &egui::Context) {
        match entry {
            #[cfg(not(target_arch = "wasm32"))]
            Entry::File => self.toggle_file_menu(ctx),
            #[cfg(target_arch = "wasm32")]
            Entry::File => self.web_files.menu_open = !self.web_files.menu_was_open,
            Entry::Tags => {
                let id = if self.tag_filter.open { "canvas.tag_filter_close" } else { "canvas.tag_filter_open" };
                if let Err(error) = self.dispatch_tag_filter(ctx, id, &json!({})) {
                    self.notice = Some(error.message);
                }
            }
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
            Entry::Automation => self.automation.open = !self.automation.open,
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
