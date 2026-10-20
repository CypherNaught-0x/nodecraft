//! The Techniques panel (#96): the project's published technique versions and the library every
//! project shares. Placing, adding and removing dispatch `technique.place` and the `library.*`
//! commands; the library list is read when the panel opens and after every change.
use super::*;
use nodecraft_types::TechniqueId;
use serde::Deserialize;
use serde_json::Value as Json;

/// The Techniques panel's state: whether it is open and the library as last listed.
#[derive(Default)]
pub(super) struct LibraryView {
    pub(super) open: bool,
    /// The last `library.list` answer; `None` reads it again on the next frame it shows.
    listed: Option<Result<Vec<Kept>, String>>,
}

/// One version the library keeps, as `library.list` describes it.
#[derive(Clone, Debug, Deserialize)]
struct Kept {
    technique: TechniqueId,
    version: u32,
    name: String,
}

#[derive(Deserialize)]
struct Listing {
    techniques: Vec<Kept>,
}

/// What the panel asked for, applied after it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Place the project's own version, by its canvas.
    Place(CanvasId),
    /// Keep the project's version in the library.
    Add(CanvasId),
    /// Place a version from the library.
    PlaceKept(TechniqueId, u32),
    Remove(TechniqueId, u32),
}

impl NodeCraftApp {
    pub(super) fn techniques_button(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        if ui
            .add(egui::Button::new(egui::RichText::new("Techniques").color(t.text_muted)).frame(false))
            .on_hover_text("Place a published technique, or keep one in the library every project shares")
            .clicked()
        {
            self.library.open = !self.library.open;
            self.library.listed = None;
        }
    }

    /// Whether this window keeps a technique library.
    fn has_library(&self) -> bool {
        self.session.enabled("library.list", &json!({})).is_ok()
    }

    /// The panel listing the project's published versions and the library's.
    pub(super) fn techniques_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.library.open {
            return;
        }
        let library = self.has_library();
        if library && self.library.listed.is_none() {
            self.library.listed = Some(self.list_library());
        }
        let mut own: Vec<(CanvasId, TechniqueId, u32, String)> = self
            .session
            .document()
            .canvases()
            .values()
            .filter_map(|canvas| canvas.technique_info().filter(|info| !info.retired).map(|info| (canvas.id(), info.id, info.version, info.name.clone())))
            .collect();
        own.sort_by(|a, b| (&a.3, a.2).cmp(&(&b.3, b.2)));
        let kept: &[Kept] = match &self.library.listed {
            Some(Ok(kept)) => kept,
            _ => &[],
        };
        let in_library = |technique: TechniqueId, version: u32| kept.iter().any(|k| k.technique == technique && k.version == version);
        let in_project = |technique: TechniqueId, version: u32| own.iter().any(|o| o.1 == technique && o.2 == version);
        let mut action = None;
        let mut open = self.library.open;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Techniques")
            .id(egui::Id::new("nodecraft.techniques"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap])
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(t.sheet_list_height).show(ui, |ui| {
                    ui.label(egui::RichText::new("This project").strong().color(t.text));
                    if own.is_empty() {
                        ui.label(egui::RichText::new("Nothing published yet. Select nodes and publish them as a technique.").color(t.text_muted));
                    }
                    for (canvas, technique, version, name) in &own {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} · v{version}", sheet_view::line(name)));
                            if ui.button("Place").on_hover_text("A node running this version, in the middle of the view").clicked() {
                                action = Some(Action::Place(*canvas));
                            }
                            if library {
                                let kept = in_library(*technique, *version);
                                let add = ui.add_enabled(!kept, egui::Button::new(if kept { "In library" } else { "Add to library" }));
                                if add.on_hover_text("Keep a copy for every project, without its example results").clicked() {
                                    action = Some(Action::Add(*canvas));
                                }
                            }
                        });
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Library").strong().color(t.text));
                    match &self.library.listed {
                        _ if !library => {
                            ui.label(egui::RichText::new("The technique library isn't available here.").color(t.text_muted));
                        }
                        Some(Err(error)) => {
                            ui.colored_label(t.danger, error);
                        }
                        Some(Ok(kept)) if kept.is_empty() => {
                            ui.label(egui::RichText::new("The library is empty. Add a version to use it in other projects.").color(t.text_muted));
                        }
                        _ => {}
                    }
                    for k in kept {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} · v{}", sheet_view::line(&k.name), k.version));
                            let hint = if in_project(k.technique, k.version) {
                                "This project has this version; a node running it, in the middle of the view"
                            } else {
                                "Copy this version into the project, and a node running it in the middle of the view"
                            };
                            if ui.add(egui::Button::new("Place")).on_hover_text(hint).clicked() {
                                action = Some(Action::PlaceKept(k.technique, k.version));
                            }
                            if ui.button("Remove").on_hover_text("Take it out of the library; projects that placed it keep their copy").clicked() {
                                action = Some(Action::Remove(k.technique, k.version));
                            }
                        });
                    }
                });
            });
        self.library.open = open;
        if let Some(action) = action {
            self.library_action(action, t);
        }
    }

    fn list_library(&mut self) -> Result<Vec<Kept>, String> {
        let listed = self.session.dispatch("library.list", &json!({})).map_err(|error| error.message)?;
        serde_json::from_value::<Listing>(listed).map(|listing| listing.techniques).map_err(|_| "The library's list couldn't be read.".to_owned())
    }

    fn library_action(&mut self, action: Action, t: &Tokens) {
        let result = match action {
            Action::Place(version) => self.place_technique(t, "technique.place", json!({"version": version})),
            Action::PlaceKept(technique, version) => ids::random_id(|bytes| self.session.document().canvases().contains_key(&CanvasId::from_bytes(*bytes)))
                .map(CanvasId::from_bytes)
                .and_then(|copy| self.place_technique(t, "library.place", json!({"technique": technique, "version": version, "copy": copy}))),
            Action::Add(canvas) => self.session.dispatch("library.add", &json!({"canvas": canvas})).map(|_| ()),
            Action::Remove(technique, version) => self.session.dispatch("library.remove", &json!({"technique": technique, "version": version})).map(|_| ()),
        };
        // What the library holds may have changed, even for a refused request (another window).
        if !matches!(action, Action::Place(_)) {
            self.library.listed = None;
        }
        match result {
            Ok(()) => self.notice = None,
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Dispatches `command` (`technique.place` or `library.place`) with `params` for a new node
    /// centred in the view, and selects it.
    fn place_technique(&mut self, t: &Tokens, command: &str, mut params: Json) -> Result<(), Error> {
        let usable = self.navigation.usable(self.viewport, t);
        let centre = self.view.to_world(self.viewport.min, usable.center());
        if !canvas::valid_rect(usable) || !centre.is_finite() {
            return Err(Error::new(ErrorKind::Conflict, "There is no canvas space to place it in."));
        }
        let id = ids::node_id(self.session.document(), ("library-place", self.session.revision()))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate a node id; try again."))?;
        let pos = json!({"x": centre.x - t.card_width / 2.0, "y": centre.y - t.card_min_height / 2.0});
        let fields = params.as_object_mut().ok_or_else(|| Error::new(ErrorKind::Internal, "placing needs its parameters"))?;
        fields.insert("canvas".into(), json!(self.canvas));
        fields.insert("id".into(), json!(id));
        fields.insert("pos".into(), pos);
        self.session.dispatch(command, &params)?;
        self.selection.only(id);
        Ok(())
    }
}
