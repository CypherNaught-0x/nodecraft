//! The Techniques panel (#96): the project's published technique versions and the library every
//! project shares. Placing, adding and removing dispatch `technique.place` and the `library.*`
//! commands; the library list is read when the panel opens and after every change. Below them,
//! the shader library (#90): each saved shader's latest version is placed with `shader.place`.
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
    pub(super) templates: Option<Result<nodecraft_engine::templates::catalog::Listing, String>>,
    template_owner: Option<std::sync::Arc<()>>,
    template_stamp: Option<u64>,
    pub(super) template_query: String,
    /// The shader library's names and latest versions, read with the techniques.
    shaders: super::shader_editor::ShaderListing,
}

impl LibraryView {
    /// Lists the libraries again the next frame the panel shows.
    pub(super) fn reset(&mut self) {
        self.listed = None;
    }
}

/// One version the library keeps, as `library.list` describes it.
#[derive(Clone, Debug, Deserialize)]
struct Kept {
    technique: TechniqueId,
    version: u32,
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct Listing {
    techniques: Vec<Kept>,
}

/// What the panel asked for, applied after it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    /// Place the project's own version, by its canvas.
    Place(CanvasId),
    AddTemplate(nodecraft_engine::templates::catalog::Identity),
    RetrieveTemplate(nodecraft_engine::templates::catalog::Identity),
    RefreshTemplates,
    /// Keep the project's version in the library.
    Add(CanvasId),
    /// Place a version from the library.
    PlaceKept(TechniqueId, u32),
    Remove(TechniqueId, u32),
    /// Place a library shader's latest version.
    PlaceShader(String),
    /// Take a shader, every version, out of the library.
    RemoveShader(String),
}

impl NodeCraftApp {
    pub(super) fn toggle_techniques(&mut self) {
        self.library.open = !self.library.open;
        self.library.listed = None;
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
        let identity = self.session.view_identity();
        let stamp = nodecraft_engine::templates::catalog::stamp(&self.session);
        if self.library.template_stamp != Some(stamp) || !self.library.template_owner.as_ref().is_some_and(|old| std::sync::Arc::ptr_eq(old, &identity)) {
            self.library.templates = None;
            self.library.template_owner = Some(identity);
            self.library.template_stamp = Some(stamp);
        }
        if self.library.templates.is_none() {
            self.library.templates = Some(
                self.session
                    .dispatch("template.list", &json!({}))
                    .map_err(|e| e.message)
                    .and_then(|value| serde_json::from_value(value).map_err(|_| "Starter templates could not be listed".into())),
            );
        }
        let can_fetch_templates = self.session.enabled("template.refresh", &json!({})).is_ok();
        let library = self.has_library();
        if library && self.library.listed.is_none() {
            self.library.listed = Some(self.list_library());
            self.library.shaders = self.library_shaders();
        }
        let mut own: Vec<(CanvasId, TechniqueId, u32, String, String)> = self
            .session
            .document()
            .canvases()
            .values()
            .filter_map(|canvas| {
                canvas
                    .technique_info()
                    .filter(|info| !info.retired)
                    .map(|info| (canvas.id(), info.id, info.version, info.name.clone(), info.description.clone()))
            })
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
                    ui.set_max_width(t.dialog_width);
                    ui.label(egui::RichText::new("This project").strong().color(t.text));
                    if own.is_empty() {
                        ui.label(egui::RichText::new("Nothing published yet. Select nodes and publish them as a technique.").color(t.text_muted));
                    }
                    for (canvas, technique, version, name, description) in &own {
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
                        if !description.is_empty() {
                            ui.label(egui::RichText::new(sheet_view::line(description)).small().color(t.text_muted));
                        }
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
                        if !k.description.is_empty() {
                            ui.label(egui::RichText::new(sheet_view::line(&k.description)).small().color(t.text_muted));
                        }
                    }
                    if let Some(listing) = &self.library.shaders {
                        ui.separator();
                        ui.label(egui::RichText::new("Shaders").strong().color(t.text));
                        let shaders: &[(String, u32)] = match listing {
                            Ok(shaders) => shaders,
                            Err(error) => {
                                ui.colored_label(t.danger, error);
                                &[]
                            }
                        };
                        if listing.is_ok() && shaders.is_empty() {
                            ui.label(egui::RichText::new("No shaders saved yet. Save one from a custom shader card's editor.").color(t.text_muted));
                        }
                        for (name, latest) in shaders {
                            ui.horizontal(|ui| {
                                ui.label(format!("{} · v{latest}", sheet_view::line(name)));
                                if ui.button("Place").on_hover_text("A custom shader card with its latest version, in the middle of the view").clicked() {
                                    action = Some(Action::PlaceShader(name.clone()));
                                }
                                if ui
                                    .button("Remove")
                                    .on_hover_text("Take it and its versions out of the library; cards placed from it keep their shader")
                                    .clicked()
                                {
                                    action = Some(Action::RemoveShader(name.clone()));
                                }
                            });
                        }
                    }
                    match &self.library.templates {
                        Some(Ok(listing)) if !listing.templates.is_empty() => {
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Starter templates").strong().color(t.text));
                                if ui.add_enabled(can_fetch_templates,egui::Button::new("Refresh templates")).clicked() {
                                    ui.ctx().request_repaint();
                                    action=Some(Action::RefreshTemplates);
                                }
                            });
                            ui.add(egui::Label::new(egui::RichText::new("Bundled examples work offline. Downloads last for this project session; added examples can be saved with your project.").small().color(t.text_muted)).wrap());
                            ui.add(egui::TextEdit::singleline(&mut self.library.template_query).hint_text("Find a template"));
                            let query = self.library.template_query.to_lowercase();
                            for entry in listing.templates.iter().filter(|entry| {
                                query.is_empty() || entry.title.to_lowercase().contains(&query) || entry.features.iter().any(|feature| feature.contains(&query))
                            }) {
                                let added = self.session.document().canvases().contains_key(&entry.canvas);
                                let identity = nodecraft_engine::templates::catalog::Identity::from(entry);
                                let available = listing.available(entry);
                                ui.horizontal(|ui| {
                                    ui.label(format!("{} · v{}",entry.title,entry.version));
                                    let label = if added {"In project"} else if available {"Add template"} else {"Retrieve template"};
                                    let enabled = !added && (available || can_fetch_templates);
                                    if ui.add_enabled(enabled,egui::Button::new(label))
                                        .on_hover_text(if added {"Open this canvas from Canvases; duplicate its nodes for another copy"}
                                            else if available {"Add this exact version as a new canvas; save its assets with your project. Undo removes the addition."}
                                            else {"Retrieve and verify this exact version without adding or running it. It stays available for this project session."})
                                        .clicked() {
                                        ui.ctx().request_repaint();
                                        action = Some(if available {Action::AddTemplate(identity)} else {Action::RetrieveTemplate(identity)});
                                    }
                                });
                                ui.add(egui::Label::new(egui::RichText::new(&entry.description).small().color(t.text_muted)).wrap());
                            }
                        }
                        Some(Err(error)) => {
                            ui.colored_label(t.danger, error);
                        }
                        _ => {}
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
            Action::RefreshTemplates => self.session.dispatch("template.refresh", &json!({})).map(|_| ()),
            Action::RetrieveTemplate(ref identity) => {
                self.session.dispatch("template.retrieve", &json!({"id":identity.id,"version":identity.version,"blake3":identity.blake3})).map(|_| ())
            }
            Action::AddTemplate(ref identity) => {
                self.session.dispatch("template.add", &json!({"id":identity.id,"version":identity.version,"blake3":identity.blake3})).and_then(|value| {
                    let canvas = serde_json::from_value(value.get("canvas").cloned().unwrap_or(Json::Null))
                        .map_err(|_| Error::new(ErrorKind::Internal, "The template canvas could not be opened"))?;
                    self.open_canvas(canvas);
                    Ok(())
                })
            }
            Action::Place(version) => self.place_technique(t, "technique.place", json!({"version": version})),
            Action::PlaceKept(technique, version) => ids::random_id(|bytes| self.session.document().canvases().contains_key(&CanvasId::from_bytes(*bytes)))
                .map(CanvasId::from_bytes)
                .and_then(|copy| self.place_technique(t, "library.place", json!({"technique": technique, "version": version, "copy": copy}))),
            Action::Add(canvas) => self.session.dispatch("library.add", &json!({"canvas": canvas})).map(|_| ()),
            Action::Remove(technique, version) => self.session.dispatch("library.remove", &json!({"technique": technique, "version": version})).map(|_| ()),
            Action::PlaceShader(ref name) => self.place_technique(t, "shader.place", json!({"name": name})),
            Action::RemoveShader(ref name) => self.session.dispatch("library.remove_shader", &json!({"name": name})).map(|_| ()),
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

    /// Dispatches `command` (`technique.place`, `library.place` or `shader.place`) with `params` for a new node
    /// centred in the view, and selects it.
    fn place_technique(&mut self, t: &Tokens, command: &str, mut params: Json) -> Result<(), Error> {
        let usable = self.navigation.usable(self.viewport, t);
        let centre = self.view.to_world(self.viewport.min, usable.center());
        if !canvas::valid_rect(usable) || !centre.is_finite() {
            return Err(Error::new(ErrorKind::Conflict, "There is no canvas space to place it in."));
        }
        let id = ids::node_id(self.session.document(), ("library-place", self.session.revision()))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate a node id; try again."))?;
        let pos = json!({"x": centre.x, "y": centre.y});
        let fields = params.as_object_mut().ok_or_else(|| Error::new(ErrorKind::Internal, "placing needs its parameters"))?;
        fields.insert("canvas".into(), json!(self.canvas));
        fields.insert("id".into(), json!(id));
        fields.insert("pos".into(), pos);
        self.centered_command(command, &mut params, id)?;
        self.selection.only(id);
        Ok(())
    }
}
