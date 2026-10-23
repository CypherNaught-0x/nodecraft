//! The Elements library (#552) from the canvas: an Element card's menu saves it to the library
//! or updates it from there, and the Elements panel places a saved Element in this project or
//! removes it. The list is read when the panel opens and after every change.
use super::*;
use serde::Deserialize;

/// The Elements panel's state: whether it is open and the library as last listed.
#[derive(Default)]
pub(super) struct ElementLibraryView {
    pub(super) open: bool,
    /// The last `library.elements` answer; `None` reads it again when next needed.
    listed: Option<Result<Vec<Listed>, String>>,
}

/// One entry, as `library.elements` describes it.
#[derive(Clone, Debug, Deserialize)]
struct Listed {
    name: String,
    category: String,
    images: usize,
    sheet: bool,
    shared: bool,
}

#[derive(Deserialize)]
struct Listing {
    elements: Vec<Listed>,
}

/// What an Element card's menu asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    /// Keep the Element in the library, over an entry of its name when `replace`.
    Save { id: NodeId, replace: bool },
    /// Bring it up to date with the library's entry of its name.
    Update(NodeId),
}

/// What the panel asked for, applied after it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Place(String),
    Remove(String),
}

impl ElementLibraryView {
    /// Whether the library (as last listed) has an entry called `name`.
    fn has(&self, name: &str) -> bool {
        matches!(&self.listed, Some(Ok(listed)) if listed.iter().any(|l| l.name.eq_ignore_ascii_case(name)))
    }
}

/// The Element card menu's library entries; what was clicked.
pub(super) fn menu(ui: &mut egui::Ui, view: &ElementLibraryView, node: &Node) -> Option<CardAction> {
    let name = nodecraft_nodes::element::name(node)?;
    let kept = view.has(name);
    let mut action = None;
    let (label, hint) = if kept {
        ("Replace in Elements library", "The library has an Element with this name: keep this one instead")
    } else {
        ("Save to Elements library", "Keep its settings and images for every project")
    };
    if ui.button(label).on_hover_text(hint).clicked() {
        action = Some(CardAction::Save { id: node.id, replace: kept });
        ui.close();
    }
    if kept && ui.button("Update from Elements library").on_hover_text("Take the library's images, category and guidelines for this name").clicked() {
        action = Some(CardAction::Update(node.id));
        ui.close();
    }
    action
}

impl NodeCraftApp {
    pub(super) fn elements_button(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        if !self.has_element_library() {
            return;
        }
        if ui
            .add(egui::Button::new(egui::RichText::new("Elements").color(t.text_muted)).frame(false))
            .on_hover_text("Place an Element saved in the library every project shares")
            .clicked()
        {
            self.element_library.open = !self.element_library.open;
            self.element_library.listed = None;
        }
    }

    /// Whether this window keeps an Elements library.
    fn has_element_library(&self) -> bool {
        self.session.enabled("library.elements", &json!({})).is_ok()
    }

    /// Reads the library's list when it isn't at hand, so Element card menus know what it holds.
    pub(super) fn element_library_listed(&mut self) -> &ElementLibraryView {
        if self.element_library.listed.is_none() && self.has_element_library() {
            let listed = self.session.dispatch("library.elements", &json!({})).map_err(|error| error.message).and_then(|listed| {
                serde_json::from_value::<Listing>(listed).map(|l| l.elements).map_err(|_| "The Elements library's list couldn't be read.".to_owned())
            });
            self.element_library.listed = Some(listed);
        }
        &self.element_library
    }

    /// The panel listing the library's Elements.
    pub(super) fn elements_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.element_library.open {
            return;
        }
        self.element_library_listed();
        let mut action = None;
        let mut open = self.element_library.open;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Elements")
            .id(egui::Id::new("nodecraft.element_library"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap])
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(t.sheet_list_height).show(ui, |ui| match &self.element_library.listed {
                    Some(Err(error)) => {
                        ui.colored_label(t.danger, error);
                    }
                    Some(Ok(listed)) if listed.is_empty() => {
                        ui.label(
                            egui::RichText::new("The library is empty. Save an Element from its card's menu to use it in other projects.").color(t.text_muted),
                        );
                    }
                    Some(Ok(listed)) => {
                        for entry in listed {
                            ui.horizontal(|ui| {
                                let images = if entry.images == 1 { "1 image".to_owned() } else { format!("{} images", entry.images) };
                                let sheet = if entry.sheet { " + sheet" } else { "" };
                                let shared = if entry.shared { " · shared" } else { "" };
                                ui.label(format!("#{} · {} · {images}{sheet}{shared}", sheet_view::line(&entry.name), entry.category));
                                if ui.button("Place").on_hover_text("Copy it into this project, in the middle of the view").clicked() {
                                    action = Some(Action::Place(entry.name.clone()));
                                }
                                if ui.button("Remove").on_hover_text("Take it out of the library; projects that placed it keep their copy").clicked() {
                                    action = Some(Action::Remove(entry.name.clone()));
                                }
                            });
                        }
                    }
                    None => {}
                });
            });
        self.element_library.open = open;
        if let Some(action) = action {
            let result = match action {
                Action::Place(name) => self.place_element(t, &name),
                Action::Remove(name) => self.session.dispatch("library.remove_element", &json!({"name": name})).map(|_| ()),
            };
            self.element_library_done(result);
        }
    }

    pub(super) fn element_library_action(&mut self, action: CardAction) {
        let result = match action {
            CardAction::Save { id, replace } => {
                self.session.dispatch("element.save", &json!({"canvas": self.canvas, "id": id, "replace": replace})).map(|_| ())
            }
            CardAction::Update(id) => match ids::node_id(self.session.document(), ("element-update", id, self.session.revision())) {
                Some(seed) => self.session.dispatch("element.update", &json!({"canvas": self.canvas, "id": id, "seed": seed})).map(|_| ()),
                None => Err(Error::new(ErrorKind::Conflict, "Couldn't allocate node ids; try again.")),
            },
        };
        self.element_library_done(result);
    }

    fn element_library_done(&mut self, result: Result<(), Error>) {
        // What the library holds may have changed, even for a refused request (another window).
        self.element_library.listed = None;
        match result {
            Ok(()) => self.notice = None,
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Places the library's `name` in the middle of the view, under a free name when an Element
    /// here has it already, and selects it.
    fn place_element(&mut self, t: &Tokens, name: &str) -> Result<(), Error> {
        let usable = self.navigation.usable(self.viewport, t);
        let centre = self.view.to_world(self.viewport.min, usable.center());
        if !canvas::valid_rect(usable) || !centre.is_finite() {
            return Err(Error::new(ErrorKind::Conflict, "There is no canvas space to place it in."));
        }
        let document = self.session.document();
        let id = ids::node_id(document, ("element-place", name, self.session.revision()))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate a node id; try again."))?;
        let canvas = document.canvases().get(&self.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "The canvas is gone."))?;
        let taken: BTreeSet<String> = canvas.nodes().values().filter_map(nodecraft_nodes::element::name).map(str::to_ascii_lowercase).collect();
        let free = (1..=1000u32)
            .map(|n| if n == 1 { name.to_owned() } else { format!("{name}-{n}") })
            .find(|candidate| candidate.len() <= nodecraft_nodes::element::MAX_NAME && !taken.contains(&candidate.to_ascii_lowercase()))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "Every name for it is taken here; rename an Element first."))?;
        let mut params =
            json!({"canvas": self.canvas, "name": name, "id": id, "pos": {"x": centre.x - t.card_width / 2.0, "y": centre.y - t.card_min_height / 2.0}});
        if free != name {
            params["as"] = json!(free);
        }
        self.session.dispatch("element.place", &params)?;
        self.selection.only(id);
        Ok(())
    }
}
