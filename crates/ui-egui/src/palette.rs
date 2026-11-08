//! The quick-add palette: double-click empty canvas to search node kinds and commands.
//! Choosing a kind creates it where the palette opened (`node.create`); choosing a command
//! dispatches it. Entries, their sections, ranking and placement are pure so they can be tested
//! without a UI.
//!
//! With nothing typed it lists sections (#1121): Recent (the last few chosen, view state only),
//! the node kinds by the media they make, Actions, then Commands, each row with its modality
//! colour, a one-line summary and its shortcut right-aligned. Typing ranks every entry together,
//! naming its section on the right. A command that can't run now is listed greyed, after those
//! that can, with the reason in place of its summary; choosing it does nothing.
use crate::theme::Tokens;
use egui::{Pos2, Rect, Vec2};
use nodecraft_types::{Error, ErrorKind, Modality};

/// Maximum visible entry rows; the complete list scrolls below them.
pub const MAX_ROWS: usize = 12;
/// How many recently chosen entries the Recent section remembers.
pub const RECENT: usize = 5;
/// Query and label characters the ranking reads; the rest are ignored, so scoring stays bounded.
const QUERY_CHARS: usize = 128;
const LABEL_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A node kind id, created with `node.create`.
    Kind(String),
    /// A command id; the app supplies supported active-canvas context or `{}`.
    Command(&'static str),
}

/// Where an entry is listed when nothing is typed, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// A node kind that isn't an action, by the media it makes; `None` for the rest (notes, decks,
    /// export, styles).
    Node(Option<Modality>),
    Action,
    Command,
}

/// The node sections' order, which is also the Actions section's order inside it.
fn modality_rank(modality: Option<Modality>) -> u8 {
    match modality {
        Some(Modality::Text) => 0,
        Some(Modality::Image) => 1,
        Some(Modality::Video) => 2,
        Some(Modality::Audio) => 3,
        Some(Modality::Mesh) => 4,
        Some(Modality::Document) => 5,
        Some(Modality::Style) => 6,
        None => 7,
    }
}

impl Section {
    fn rank(self) -> u8 {
        match self {
            Section::Node(modality) => modality_rank(modality),
            Section::Action => 8,
            Section::Command => 9,
        }
    }

    /// The section's heading.
    pub fn title(self) -> &'static str {
        match self {
            Section::Node(Some(modality)) => crate::noodle::modality_noun(modality),
            Section::Node(None) => "Other",
            Section::Action => "Actions",
            Section::Command => "Commands",
        }
    }

    /// What a ranked row says it is, on its right.
    pub fn hint(self) -> &'static str {
        match self {
            Section::Node(Some(modality)) => crate::noodle::modality_noun(modality),
            Section::Node(None) => "Node",
            Section::Action => "Action",
            Section::Command => "Command",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub target: Target,
    pub label: String,
    /// The key that does the same from the canvas, if any.
    pub shortcut: Option<String>,
    pub section: Section,
    /// The media colour of its dot: the media a kind makes, or an action works on.
    pub modality: Option<Modality>,
    /// One line under the label, from the kind's or command's metadata.
    pub summary: String,
    /// Why a command can't run now. It is listed greyed with this in place of its summary and
    /// is never chosen.
    pub unavailable: Option<String>,
}

impl Entry {
    /// A node kind's entry: kinds that aren't actions are listed by the media they make, actions
    /// by the media they work on. A kind without a summary (a plugin's) shows its ports.
    pub fn kind(descriptor: &nodecraft_nodes::NodeDescriptor) -> Self {
        let output = descriptor.outputs.first().map(|port| port.port_type.modality);
        let input = descriptor.inputs.first().map(|port| port.port_type.modality);
        let (section, modality) = if descriptor.category == nodecraft_nodes::Category::Action {
            (Section::Action, input.or(output))
        } else {
            // Styles are few and feed images: listed with the other nodes rather than alone.
            (Section::Node(output.filter(|m| *m != Modality::Style)), output)
        };
        let summary = match descriptor.summary() {
            Some(summary) => summary.to_owned(),
            None => ports_summary(input, output),
        };
        Self {
            target: Target::Kind(descriptor.id.clone()),
            label: descriptor.label.clone(),
            shortcut: descriptor.quick_add.clone(),
            section,
            modality,
            summary,
            unavailable: None,
        }
    }

    /// A command's entry, with why it can't run now when it can't.
    pub fn command(spec: &nodecraft_engine::commands::CommandSpec, unavailable: Option<&Error>) -> Self {
        Self {
            target: Target::Command(spec.id),
            label: spec.label.to_owned(),
            shortcut: spec.shortcut.map(str::to_owned),
            section: Section::Command,
            modality: None,
            summary: spec.summary().unwrap_or_default().to_owned(),
            unavailable: unavailable.map(|error| sentence(&error.message)),
        }
    }

    pub fn available(&self) -> bool {
        self.unavailable.is_none()
    }

    /// The line under the label: the summary, or why it can't run now.
    pub fn description(&self) -> &str {
        self.unavailable.as_deref().unwrap_or(&self.summary)
    }
}

/// "Image → Video" for a kind with no summary of its own.
fn ports_summary(input: Option<Modality>, output: Option<Modality>) -> String {
    match (input, output) {
        (Some(input), Some(output)) => format!("{} → {}", crate::noodle::modality_noun(input), crate::noodle::modality_noun(output)),
        (None, Some(output)) => format!("Makes {}", crate::noodle::modality_noun(output)),
        (Some(input), None) => format!("Takes {}", crate::noodle::modality_noun(input)),
        (None, None) => String::new(),
    }
}

/// An error message as a row reads it: its first letter capitalised.
fn sentence(message: &str) -> String {
    let mut chars = message.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether a command's `enabled` error says why it can't run *now*, so the palette lists it
/// greyed with that reason. Missing or wrong parameters (the command needs some), a missing
/// target and a host that can't run it at all leave it out, as before.
pub fn explains(error: &Error) -> bool {
    matches!(error.kind, ErrorKind::Conflict | ErrorKind::Busy | ErrorKind::Forbidden | ErrorKind::Budget | ErrorKind::Authentication)
}

/// Puts entries in their listing order: by section (actions by the media they work on), the
/// commands that can run before those that can't, then by label.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| a.label.cmp(&b.label)));
}

fn key(entry: &Entry) -> (u8, u8, bool) {
    let inner = if entry.section == Section::Action { modality_rank(entry.modality) } else { 0 };
    (entry.section.rank(), inner, entry.unavailable.is_some())
}

/// One line of the list: a section heading or an entry (by index), with its section on the right
/// when the list is ranked or recent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Header(&'static str),
    Row { entry: usize, hint: bool },
}

/// The list for `query`: sections when it is empty, otherwise every match ranked best first,
/// those that can't run last. `recent` is most recent first.
pub fn layout(entries: &[Entry], query: &str, recent: &[Target]) -> Vec<Item> {
    let query: String = query.trim().to_lowercase().chars().take(QUERY_CHARS).collect();
    let mut items = Vec::new();
    if query.is_empty() {
        let mut seen = Vec::new();
        for target in recent.iter().take(RECENT) {
            if let Some(index) = entries.iter().position(|entry| &entry.target == target && entry.available())
                && !seen.contains(&index)
            {
                seen.push(index);
            }
        }
        if !seen.is_empty() {
            items.push(Item::Header("Recent"));
            items.extend(seen.into_iter().map(|entry| Item::Row { entry, hint: true }));
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|a, b| match (entries.get(*a), entries.get(*b)) {
            (Some(x), Some(y)) => key(x).cmp(&key(y)),
            _ => std::cmp::Ordering::Equal,
        });
        let mut current = None;
        for index in order {
            let Some(entry) = entries.get(index) else { continue };
            let title = entry.section.title();
            if current != Some(title) {
                items.push(Item::Header(title));
                current = Some(title);
            }
            items.push(Item::Row { entry: index, hint: false });
        }
        return items;
    }
    let mut ranked: Vec<(bool, std::cmp::Reverse<u32>, u8, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| score(&query, entry).map(|score| (entry.unavailable.is_some(), std::cmp::Reverse(score), entry.section.rank(), index)))
        .collect();
    ranked.sort();
    items.extend(ranked.into_iter().map(|(_, _, _, entry)| Item::Row { entry, hint: true }));
    items
}

/// The entries of `items` that can be highlighted, in order.
pub fn rows(items: &[Item]) -> Vec<usize> {
    items.iter().filter_map(|item| if let Item::Row { entry, .. } = item { Some(*entry) } else { None }).collect()
}

/// How well `query` (lowercase, trimmed, non-empty) matches an entry; higher is better, `None` is
/// no match. Tiers, best first: the whole label, a label prefix, a word prefix, inside the label,
/// inside the id, three or more of the query's letters in order through the label (closer
/// together is better), then inside the summary. Within a tier a shorter label wins. Work is
/// bounded by `QUERY_CHARS` × `LABEL_CHARS`, and the same input always scores the same.
pub fn score(query: &str, entry: &Entry) -> Option<u32> {
    let label: String = entry.label.chars().take(LABEL_CHARS).collect::<String>().to_lowercase();
    let extra = label.chars().count().saturating_sub(query.chars().count()).min(99);
    let tie = 99u32.saturating_sub(u32::try_from(extra).unwrap_or(99));
    let id = match &entry.target {
        Target::Kind(id) => id.to_lowercase(),
        Target::Command(id) => id.to_lowercase(),
    };
    let tier = if label == query {
        9
    } else if label.starts_with(query) {
        8
    } else if label.split(|c: char| !c.is_alphanumeric()).any(|word| word.starts_with(query)) {
        7
    } else if label.contains(query) {
        6
    } else if id.contains(query) {
        5
    } else if let Some(closeness) = subsequence(query, &label) {
        return Some(100 + closeness);
    } else if query.chars().count() >= 3 && entry.summary.chars().take(LABEL_CHARS).collect::<String>().to_lowercase().contains(query) {
        return Some(tie / 2);
    } else {
        return None;
    };
    Some(tier * 100 + tie)
}

/// The query's letters (spaces aside) in order through `label`: 0..=399, higher when they sit
/// closer together and nearer the start.
fn subsequence(query: &str, label: &str) -> Option<u32> {
    let wanted: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    // One or two letters scattered through a label match nearly everything.
    if wanted.len() < 3 {
        return None;
    }
    let mut next = 0;
    let (mut first, mut last) = (None, 0usize);
    for (at, c) in label.chars().enumerate() {
        if wanted.get(next) == Some(&c) {
            first.get_or_insert(at);
            last = at;
            next += 1;
            if next == wanted.len() {
                break;
            }
        }
    }
    let first = first?;
    if next < wanted.len() {
        return None;
    }
    let gaps = last.saturating_sub(first).saturating_add(1).saturating_sub(wanted.len());
    let penalty = gaps.saturating_mul(10).saturating_add(first).min(399);
    Some(399u32.saturating_sub(u32::try_from(penalty).unwrap_or(399)))
}

/// A row's accessible name: its label, then its shortcut as this platform spells it,
/// `Undo (Ctrl+Z)` or `Undo (⌘Z)`.
pub fn row_name(ctx: &egui::Context, entry: &Entry) -> String {
    match &entry.shortcut {
        Some(key) => crate::shortcut::with_shortcut(ctx, &entry.label, key),
        None => entry.label.clone(),
    }
}

/// A row's shortcut, right-aligned in monospace: `text_muted` at rest, the selection colour on
/// the highlighted row so it keeps its contrast there.
pub(crate) fn shortcut_chip(ctx: &egui::Context, shortcut: &str, selected: bool, t: &Tokens) -> egui::RichText {
    let text = egui::RichText::new(crate::shortcut::format_shortcut(ctx, shortcut)).monospace().size(t.label_size);
    if selected { text } else { text.color(t.text_muted) }
}

/// Where the popup goes: its top-left and the height its list gets. It keeps `width` and `want`
/// (its full height) when they fit below `anchor` inside `content` less `margin`; with no room
/// below it opens upward from `anchor` when there's more room above, and otherwise takes the
/// room below, scrolling its list. `chrome` is the part of the height that isn't the list.
pub fn place(anchor: Pos2, width: f32, want: f32, chrome: f32, content: Rect, margin: f32) -> (Pos2, f32) {
    let room = if content.width() > margin * 2.0 && content.height() > margin * 2.0 { content.shrink(margin) } else { content };
    let x = anchor.x.min(room.right() - width).max(room.left());
    let y = anchor.y.min(room.bottom()).max(room.top());
    let (below, above) = (room.bottom() - y, y - room.top());
    let list = (want - chrome).max(0.0);
    if want <= below {
        (Pos2::new(x, y), list)
    } else if above > below {
        let height = want.min(above);
        (Pos2::new(x, y - height), (height - chrome).max(0.0))
    } else {
        (Pos2::new(x, y), (below - chrome).max(0.0))
    }
}

/// The Recent section's memory: egui temporary data, so it is view state for this window, never
/// saved in the document and gone on restart.
fn recent_id() -> egui::Id {
    egui::Id::new("quick_add_palette_recent")
}

/// The recently chosen entries, most recent first.
pub fn recent(ctx: &egui::Context) -> Vec<Target> {
    ctx.data(|data| data.get_temp::<Vec<Target>>(recent_id())).unwrap_or_default()
}

/// Remembers `target` as the most recent choice, keeping at most [`RECENT`].
pub fn remember(ctx: &egui::Context, target: &Target) {
    ctx.data_mut(|data| {
        let list = data.get_temp_mut_or_default::<Vec<Target>>(recent_id());
        list.retain(|known| known != target);
        list.insert(0, target.clone());
        list.truncate(RECENT);
    });
}

/// An open palette: where it was opened and what is typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// World position new nodes go to.
    pub world: Pos2,
    /// Screen position it was opened at: the popup's top-left when there's room below.
    pub screen: Pos2,
    pub query: String,
    /// The highlighted row among the list's entries (headings aside).
    pub highlighted: usize,
    focus: bool,
    /// Last frame's list scroll offset and the popup's height outside its list.
    scroll: f32,
    chrome: Option<f32>,
}

pub enum Outcome {
    Open,
    Closed,
    Chosen(Target),
}

/// Heights of a heading and an entry row, from the current fonts.
fn heights(ctx: &egui::Context, t: &Tokens) -> (f32, f32) {
    let style = ctx.global_style();
    let body = egui::TextStyle::Body.resolve(&style);
    let small = egui::TextStyle::Small.resolve(&style);
    let (body, small) = ctx.fonts_mut(|fonts| (fonts.row_height(&body), fonts.row_height(&small)));
    let pad = t.control_vertical_padding * 2.0;
    (small + t.control_gap * 2.0, body + small + pad * 2.0)
}

impl Palette {
    pub(crate) fn query_id() -> egui::Id {
        egui::Id::new("quick_add_palette_query")
    }

    pub(crate) fn owns_text_focus(ctx: &egui::Context) -> bool {
        ctx.memory(|memory| memory.has_focus(Self::query_id()))
    }

    pub(crate) fn release_text_focus(ctx: &egui::Context) {
        ctx.memory_mut(|memory| memory.surrender_focus(Self::query_id()));
    }

    pub fn new(world: Pos2, screen: Pos2) -> Self {
        Self { world, screen, query: String::new(), highlighted: 0, focus: true, scroll: 0.0, chrome: None }
    }

    /// One frame of the popup: ↑/↓ move, ↵ chooses, Escape or a click elsewhere closes. Choosing
    /// a command that can't run now does nothing.
    pub fn show(&mut self, ctx: &egui::Context, entries: &[Entry], t: &Tokens) -> Outcome {
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if escape {
            return Outcome::Closed;
        }
        let recent = recent(ctx);
        let mut items = layout(entries, &self.query, &recent);
        let last = rows(&items).len().saturating_sub(1);
        if down {
            self.highlighted = self.highlighted.saturating_add(1).min(last);
        }
        if up {
            self.highlighted = self.highlighted.saturating_sub(1);
        }
        self.highlighted = self.highlighted.min(last);
        // The double-click that opened the palette must not count as a click elsewhere.
        let first_frame = self.focus;
        let (header_height, row_height) = heights(ctx, t);
        let item_height = |item: &Item| if matches!(item, Item::Header(_)) { header_height } else { row_height };
        // Placed for the whole list, not this query's matches, so the query field stays where it
        // opened while typing narrows the list.
        let full: f32 = layout(entries, "", &recent).iter().map(item_height).sum();
        let margin = t.control_gap * 2.0;
        let content = ctx.content_rect();
        let width = t.dialog_width.min((content.width() - t.navigation_margin * 2.0).max(t.card_width.min(content.width())));
        // The query field and the frame; measured after the first frame.
        let chrome = self.chrome.unwrap_or(margin * 2.0 + ctx.global_style().spacing.interact_size.y * 2.0);
        let want = chrome + full.min(row_height * MAX_ROWS as f32).max(row_height);
        let (top_left, list_height) = place(self.screen, width, want, chrome, content, t.navigation_margin);
        let mut chosen = None;
        let mut scrolled = None;
        let area = egui::Area::new(egui::Id::new("quick_add_palette")).order(egui::Order::Foreground).constrain(false).fixed_pos(top_left).show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.panel_fill)
                .stroke(egui::Stroke::new(t.stroke_hairline, t.panel_stroke))
                .corner_radius(t.panel_radius)
                .inner_margin(margin)
                .show(ui, |ui| {
                    ui.set_width((width - margin * 2.0 - t.stroke_hairline * 2.0).max(0.0));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .id(Self::query_id())
                            .hint_text("Add a node or run a command…")
                            .desired_width(f32::INFINITY)
                            .char_limit(QUERY_CHARS),
                    );
                    // Keep typing going to the query, even after a click on the popup's padding.
                    if self.focus || ui.ctx().memory(|m| m.focused().is_none()) {
                        edit.request_focus();
                        self.focus = false;
                    }
                    if edit.changed() {
                        self.highlighted = 0;
                        items = layout(entries, &self.query, &recent);
                    }
                    let rows_now = rows(&items);
                    if rows_now.is_empty() {
                        ui.label(egui::RichText::new("Nothing matches").color(t.text_muted));
                    }
                    // Item tops in the list, and the highlighted row's (with its heading when it
                    // starts a section), to keep it in view.
                    let mut tops = Vec::with_capacity(items.len());
                    let mut y = 0.0;
                    let mut row = 0usize;
                    let mut highlight = (0.0, 0.0);
                    for (index, item) in items.iter().enumerate() {
                        tops.push(y);
                        if matches!(item, Item::Row { .. }) {
                            if row == self.highlighted {
                                let header = index.checked_sub(1).and_then(|i| items.get(i)).is_some_and(|i| matches!(i, Item::Header(_)));
                                highlight = (if header { y - header_height } else { y }, y + row_height);
                            }
                            row += 1;
                        }
                        y += item_height(item);
                    }
                    let total = y;
                    let mut offset = self.scroll;
                    if first_frame || edit.changed() {
                        offset = 0.0;
                    } else if up || down {
                        if highlight.0 < offset {
                            offset = highlight.0;
                        } else if highlight.1 > offset + list_height {
                            offset = highlight.1 - list_height;
                        }
                    }
                    // The area's room is last frame's size: ask for the list's height outright.
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt("entries")
                        .auto_shrink([false, true])
                        .max_height(list_height)
                        .min_scrolled_height(list_height.min(total));
                    if first_frame || up || down || edit.changed() {
                        scroll = scroll.vertical_scroll_offset(offset.max(0.0));
                    }
                    let output = scroll.show_viewport(ui, |ui, viewport| {
                        ui.set_height(total);
                        let origin = ui.max_rect().min;
                        let width = ui.max_rect().width();
                        let mut row = 0usize;
                        for (index, item) in items.iter().enumerate() {
                            let top = tops.get(index).copied().unwrap_or_default();
                            let height = item_height(item);
                            let position = row;
                            if matches!(item, Item::Row { .. }) {
                                row += 1;
                            }
                            if top + height < viewport.min.y || top > viewport.max.y {
                                continue;
                            }
                            let rect = Rect::from_min_size(origin + Vec2::new(0.0, top), Vec2::new(width, height));
                            match *item {
                                Item::Header(title) => header(ui, rect, title, t),
                                Item::Row { entry: at, hint } => {
                                    let Some(entry) = entries.get(at) else { continue };
                                    let selected = position == self.highlighted;
                                    let response = paint_row(ui, rect, entry, selected, hint, position, t);
                                    // Follow the pointer only when it moves, so resting it doesn't fight ↑/↓.
                                    if response.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                                        self.highlighted = position;
                                    }
                                    if response.clicked() && entry.available() {
                                        chosen = Some(entry.target.clone());
                                    }
                                }
                            }
                        }
                    });
                    scrolled = Some((output.state.offset.y, output.inner_rect.height(), rows_now.is_empty()));
                });
        });
        if let Some((offset, list, empty)) = scrolled {
            self.scroll = offset;
            // "Nothing matches" isn't part of the frame around the list.
            if !empty {
                self.chrome = Some((area.response.rect.height() - list).max(0.0));
            }
        }
        if let Some(target) = chosen {
            remember(ctx, &target);
            return Outcome::Chosen(target);
        }
        // ↵ chooses from the query as it is after this frame's typing (a paste and ↵ can arrive
        // in the same frame), not the list laid out before it.
        if enter {
            let fresh = rows(&layout(entries, &self.query, &recent));
            let row = self.highlighted.min(fresh.len().saturating_sub(1));
            if let Some(entry) = fresh.get(row).and_then(|index| entries.get(*index))
                && entry.available()
            {
                remember(ctx, &entry.target);
                return Outcome::Chosen(entry.target.clone());
            }
        }
        if !first_frame && area.response.clicked_elsewhere() {
            return Outcome::Closed;
        }
        Outcome::Open
    }
}

/// A section heading: small `text_muted` text, named for screen readers.
fn header(ui: &mut egui::Ui, rect: Rect, title: &'static str, t: &Tokens) {
    let response = ui.interact(rect, ui.id().with(("palette_header", title)), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title));
    let galley = egui::WidgetText::from(egui::RichText::new(title).size(t.label_size).strong().color(t.text_muted)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Small,
    );
    let pos = Pos2::new(rect.left() + ui.spacing().button_padding.x, rect.bottom() - galley.size().y - t.control_vertical_padding);
    ui.painter().galley(pos, galley, t.text_muted);
}

/// One entry: its modality dot, label and shortcut on the first line, its summary (or why it
/// can't run) and, when ranked, its section on the second. Greyed when it can't run.
fn paint_row(ui: &mut egui::Ui, rect: Rect, entry: &Entry, selected: bool, hint: bool, position: usize, t: &Tokens) -> egui::Response {
    let response = ui.interact(rect, ui.id().with(("palette_row", position)), egui::Sense::click());
    // Named before `widget_info`, whose closure runs under the context's lock.
    let name = row_name(ui.ctx(), entry);
    let available = entry.available();
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, available, selected, &name));
    let description = entry.description().to_owned();
    if !description.is_empty() {
        ui.ctx().accesskit_node_builder(response.id, |node| node.set_description(description.clone()));
    }
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, t.panel_radius, t.selection_fill);
    } else if response.hovered() {
        painter.rect_filled(rect, t.panel_radius, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    // Greyed when it can't run, highlighted or not.
    let (strong, muted) = match (selected, available) {
        (_, false) => (t.text_muted, t.text_muted),
        (true, true) => (t.selection_text, t.selection_text),
        (false, true) => (t.text, t.text_muted),
    };
    let pad = ui.spacing().button_padding.x;
    let inner = rect.shrink2(Vec2::new(pad, t.control_vertical_padding * 2.0));
    let dot = t.tag_identity_size;
    let text_left = inner.left() + dot + t.control_gap * 2.0;
    if let Some(modality) = entry.modality {
        let center = Pos2::new(inner.left() + dot * 0.5, inner.top() + inner.height() * 0.25 + t.control_vertical_padding);
        let color = if available { t.wire(modality) } else { t.text_muted };
        painter.circle_filled(center, dot * 0.5, color);
    }
    let galley = |text: egui::RichText, width: f32, style: egui::TextStyle| {
        egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), width.max(0.0), style)
    };
    let full = (inner.right() - text_left).max(0.0);
    let line = inner.top() + (inner.height() * 0.5);
    // First line: the label, the shortcut against the right edge.
    let key = entry.shortcut.as_deref().map(|key| galley(shortcut_chip(ui.ctx(), key, selected, t).color(muted), full * 0.5, egui::TextStyle::Monospace));
    let key_width = key.as_ref().map_or(0.0, |key| key.size().x + t.control_gap * 2.0);
    let label = galley(egui::RichText::new(&entry.label).color(strong), full - key_width, egui::TextStyle::Body);
    let label_pos = Pos2::new(text_left, line - label.size().y);
    if let Some(key) = key {
        let pos = Pos2::new(inner.right() - key.size().x, line - label.size().y * 0.5 - key.size().y * 0.5);
        painter.galley(pos, key, muted);
    }
    painter.galley(label_pos, label, strong);
    // Second line: the summary or reason, the section on the right when ranked.
    let section = hint.then(|| galley(egui::RichText::new(entry.section.hint()).size(t.label_size).color(muted), full * 0.4, egui::TextStyle::Small));
    let section_width = section.as_ref().map_or(0.0, |section| section.size().x + t.control_gap * 2.0);
    if let Some(section) = section {
        painter.galley(Pos2::new(inner.right() - section.size().x, line), section, muted);
    }
    if !entry.description().is_empty() {
        let text = galley(egui::RichText::new(entry.description()).size(t.label_size).color(muted), full - section_width, egui::TextStyle::Small);
        painter.galley(Pos2::new(text_left, line), text, muted);
    }
    if !available {
        return response.on_hover_text(entry.description());
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(id: &str, label: &str, key: Option<&str>) -> Entry {
        Entry {
            target: Target::Kind(id.into()),
            label: label.into(),
            shortcut: key.map(str::to_owned),
            section: Section::Node(Some(Modality::Image)),
            modality: Some(Modality::Image),
            summary: String::new(),
            unavailable: None,
        }
    }

    fn command(id: &'static str, label: &str, unavailable: Option<&str>) -> Entry {
        Entry {
            target: Target::Command(id),
            label: label.into(),
            shortcut: None,
            section: Section::Command,
            modality: None,
            summary: "Does a thing".into(),
            unavailable: unavailable.map(str::to_owned),
        }
    }

    fn labels<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a str> {
        rows(&layout(entries, query, &[])).into_iter().filter_map(|i| entries.get(i)).map(|e| e.label.as_str()).collect()
    }

    #[test]
    fn ranking_prefers_exact_then_prefixes_words_inside_and_letters_in_order() {
        let entries = vec![
            kind("source.text", "Text", Some("T")),
            kind("source.image", "Image", Some("I")),
            kind("test.context", "Image context", None),
            kind("image.blur_more", "Blur more", None),
            kind("image.blur", "Blur", None),
            kind("test.motion", "Motion blur", None),
            command("edit.undo", "Undo", None),
        ];
        // Label matches come first; an id match ("image.blur") follows.
        assert_eq!(labels(&entries, "ima"), vec!["Image", "Image context", "Blur", "Blur more"]);
        assert_eq!(labels(&entries, "CONTEXT"), vec!["Image context"]);
        assert_eq!(labels(&entries, "ext"), vec!["Text", "Image context"]);
        assert_eq!(labels(&entries, "edit.u"), vec!["Undo"]);
        assert_eq!(labels(&entries, "blur"), vec!["Blur", "Blur more", "Motion blur"]);
        // Letters in order: "imc" reaches Image context, not Image.
        assert_eq!(labels(&entries, "imc"), vec!["Image context"]);
        assert!(labels(&entries, "zzz").is_empty());
        // Unicode, odd and overlong input never panics and is deterministic.
        assert!(labels(&entries, "ß̶\u{0}").is_empty());
        let long = "b".repeat(10_000);
        assert_eq!(labels(&entries, &long), labels(&entries, &long));
    }

    #[test]
    fn an_empty_query_lists_sections_with_recent_first_and_unavailable_commands_last() {
        let mut entries = vec![
            command("edit.undo", "Undo", Some("Nothing to undo")),
            command("perf.start", "Start capture", None),
            Entry { section: Section::Action, ..kind("image.blur", "Blur", None) },
            kind("source.image", "Image", None),
            Entry { section: Section::Node(Some(Modality::Text)), modality: Some(Modality::Text), ..kind("source.text", "Text", None) },
        ];
        sort(&mut entries);
        let items = layout(&entries, "", &[Target::Kind("image.blur".into()), Target::Command("edit.undo")]);
        let shown: Vec<String> = items
            .iter()
            .map(|item| match item {
                Item::Header(title) => format!("# {title}"),
                Item::Row { entry, .. } => entries.get(*entry).map(|e| e.label.clone()).unwrap_or_default(),
            })
            .collect();
        // An unavailable command isn't offered as recent.
        assert_eq!(shown, ["# Recent", "Blur", "# Text", "Text", "# Image", "Image", "# Actions", "Blur", "# Commands", "Start capture", "Undo"]);
        // Typed, the unavailable command comes after every runnable match.
        let both = vec![command("edit.undo", "Undo", Some("Nothing to undo")), command("edit.underline", "Underline", None)];
        assert_eq!(labels(&both, "und"), vec!["Underline", "Undo"]);
    }

    #[test]
    fn placement_opens_upward_and_stays_inside_the_window() {
        let content = Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0));
        // Room below: it opens at the pointer.
        assert_eq!(place(Pos2::new(100.0, 100.0), 440.0, 500.0, 50.0, content, 16.0), (Pos2::new(100.0, 100.0), 450.0));
        // Bottom-right corner: it opens upward and left, whole.
        let (at, list) = place(Pos2::new(1430.0, 890.0), 440.0, 500.0, 50.0, content, 16.0);
        assert_eq!((at, list), (Pos2::new(1440.0 - 16.0 - 440.0, 884.0 - 500.0), 450.0));
        // Too tall for either side: the larger side, its list scrolling.
        let small = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 400.0));
        let (at, list) = place(Pos2::new(10.0, 300.0), 440.0, 900.0, 50.0, small, 16.0);
        assert_eq!(at.y, 16.0);
        assert!((list - (284.0 - 50.0)).abs() < 0.01);
        // A degenerate window never panics or yields a negative list.
        let (_, list) = place(Pos2::new(f32::NAN, 5.0), 440.0, 900.0, 50.0, Rect::from_min_size(Pos2::ZERO, Vec2::new(10.0, 10.0)), 16.0);
        assert!(list >= 0.0);
    }

    #[test]
    fn recent_keeps_the_last_five_most_recent_first() {
        let ctx = egui::Context::default();
        for i in 0..7 {
            remember(&ctx, &Target::Kind(format!("k{i}")));
        }
        remember(&ctx, &Target::Kind("k3".into()));
        let ids: Vec<Target> = recent(&ctx);
        assert_eq!(ids, ["k3", "k6", "k5", "k4", "k2"].map(|id| Target::Kind(id.into())));
    }
}
