//! The Techniques panel (#96): the project's published technique versions and the library every
//! project shares. Placing, adding and removing dispatch `technique.place` and the `library.*`
//! commands; the library list is read when the panel opens and after every change. Below them,
//! the shader library (#90): each saved shader's latest version is placed with `shader.place`.
//! The panel's other tabs browse the project's assets, past results and trained styles
//! (`library_browsers`).
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
    /// The Assets, History and Styles tabs.
    pub(super) browsers: super::library_browsers::Browsers,
    /// A removal waiting for its second click (#1111), and whether Keep still needs focus.
    pub(super) confirm: Option<Confirm>,
    confirm_focus: bool,
    /// Whether the question was drawn last frame: Escape answers it only then, and a question
    /// whose row is gone (another tab, the item removed elsewhere) is dropped.
    confirm_shown: bool,
}

/// A removal from outside the document's undo history, asked about inline before it happens
/// (#1111). Commands stay non-interactive: only the panel asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Confirm {
    Technique(TechniqueId, u32),
    Shader(String),
    ClearTemplates,
}

/// The inline question: "Remove *name*?", then **Remove** and **Keep**, with focus on Keep the
/// first frame so Enter or Space keeps it. `Some(true)` removes, `Some(false)` keeps.
fn confirm_row(ui: &mut egui::Ui, question: &str, hover: &str, focus: &mut bool, t: &Tokens) -> Option<bool> {
    let mut answer = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(question).color(t.text));
        if ui.button("Remove").on_hover_text(hover).clicked() {
            answer = Some(true);
        }
        let keep = ui.button("Keep");
        if std::mem::take(focus) {
            keep.request_focus();
        }
        if keep.clicked() {
            answer = Some(false);
        }
    });
    answer
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
    /// Cards in it that name a Soul ID, which works only on the account that trained it.
    #[serde(default)]
    account_bound: usize,
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
    /// `templates.update` (#954).
    UpdateTemplates,
    /// `templates.clear_cache`.
    ClearTemplates,
    /// Keep the project's version in the library.
    Add(CanvasId),
    /// Place a version from the library.
    PlaceKept(TechniqueId, u32),
    Remove(TechniqueId, u32),
    /// Place a library shader's latest version.
    PlaceShader(String),
    /// Take a shader, every version, out of the library.
    RemoveShader(String),
    /// Ask before a removal (#1111); `Keep` answers no.
    Ask(Confirm),
    Keep,
}

impl NodeCraftApp {
    pub(super) fn toggle_techniques(&mut self) {
        self.library.open = !self.library.open;
        self.library.listed = None;
        self.library.confirm = None;
    }

    /// Whether this window keeps a technique library.
    fn has_library(&self) -> bool {
        self.session.enabled("library.list", &json!({})).is_ok()
    }

    /// The panel listing the project's published versions and the library's.
    pub(super) fn techniques_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        // A library import keeps going, and shows its progress, with the panel closed.
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_library_import(ctx, t);
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
        // The engine's own reason, shown on the disabled Update templates button (#1113).
        let fetch_reason = self.session.enabled("template.refresh", &json!({})).map_err(|error| error.message);
        let can_fetch_templates = fetch_reason.is_ok();
        let template_status = self.template_status();
        let template_waiting = self.template_fetch.as_ref().is_some_and(template_fetch::TemplateFetcher::waiting)
            || nodecraft_engine::templates::disk::is_clearing(&self.session)
            || nodecraft_engine::templates::disk::is_loading(&self.session);
        let can_clear_templates = !template_waiting
            && self.session.enabled("templates.clear_cache", &json!({})).is_ok()
            && nodecraft_engine::templates::catalog::cached(&self.session) > 0;
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
        self.refresh_browser();
        let selected = self.browse_selected_card();
        let mut browse = None;
        let kept: &[Kept] = match &self.library.listed {
            Some(Ok(kept)) => kept,
            _ => &[],
        };
        let in_library = |technique: TechniqueId, version: u32| kept.iter().any(|k| k.technique == technique && k.version == version);
        let in_project = |technique: TechniqueId, version: u32| own.iter().any(|o| o.1 == technique && o.2 == version);
        let mut action = None;
        let mut open = self.library.open;
        // Escape answers a pending removal with Keep while its question is on screen, and is
        // consumed so the panels and popups drawn after this one don't also act on it.
        if self.library.confirm.is_some() && self.library.confirm_shown && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.library.confirm = None;
        }
        let confirm = self.library.confirm.clone();
        let mut confirm_focus = self.library.confirm_focus;
        let mut confirm_shown = false;
        let cached_templates = nodecraft_engine::templates::catalog::cached(&self.session);
        crate::floating::panel(ctx, "Techniques", egui::Id::new("nodecraft.techniques"), crate::floating::Home::RightTop, t).open(&mut open).show(ctx, |ui| {
            ui.set_max_width(t.dialog_width);
            super::library_browsers::tabs(ui, &mut self.library.browsers);
            if self.library.browsers.tab != super::library_browsers::Tab::Library {
                browse = super::library_browsers::show(ui, t, &mut self.library.browsers, &mut self.previews, &self.session, self.canvas, selected);
                return;
            }
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
                        if confirm == Some(Confirm::Technique(k.technique, k.version)) {
                            let question = format!("Remove {} v{}?", sheet_view::line(&k.name), k.version);
                            let hover = "Take it out of the library; projects that placed it keep their copy";
                            confirm_shown = true;
                            action = confirm_row(ui, &question, hover, &mut confirm_focus, t)
                                .map(|remove| if remove { Action::Remove(k.technique, k.version) } else { Action::Keep })
                                .or(action.take());
                            return;
                        }
                        if ui.add(egui::Button::new("Place")).on_hover_text(hint).clicked() {
                            action = Some(Action::PlaceKept(k.technique, k.version));
                        }
                        if ui.button("Remove").on_hover_text("Take it out of the library; projects that placed it keep their copy").clicked() {
                            action = Some(Action::Ask(Confirm::Technique(k.technique, k.version)));
                        }
                    });
                    if !k.description.is_empty() {
                        ui.label(egui::RichText::new(sheet_view::line(&k.description)).small().color(t.text_muted));
                    }
                    if k.account_bound > 0 {
                        ui.label(
                            egui::RichText::new("Uses a Higgsfield Soul ID: it runs only on the account that trained it, in any project")
                                .small()
                                .color(t.text_muted),
                        );
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
                            if confirm.as_ref() == Some(&Confirm::Shader(name.clone())) {
                                let question = format!("Remove {} and its versions?", sheet_view::line(name));
                                let hover = "Take it out of the library; cards placed from it keep their shader";
                                confirm_shown = true;
                            action = confirm_row(ui, &question, hover, &mut confirm_focus, t)
                                    .map(|remove| if remove { Action::RemoveShader(name.clone()) } else { Action::Keep })
                                    .or(action.take());
                                return;
                            }
                            ui.label(format!("{} · v{latest}", sheet_view::line(name)));
                            if ui.button("Place").on_hover_text("A custom shader card with its latest version, in the middle of the view").clicked() {
                                action = Some(Action::PlaceShader(name.clone()));
                            }
                            if ui
                                .button("Remove")
                                .on_hover_text("Take it and its versions out of the library; cards placed from it keep their shader")
                                .clicked()
                            {
                                action = Some(Action::Ask(Confirm::Shader(name.clone())));
                            }
                        });
                    }
                }
                match &self.library.templates {
                    Some(Ok(listing)) if !listing.templates.is_empty() => {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Starter templates").strong().color(t.text));
                            let update = if template_waiting {
                                Err("Templates are being updated, read or cleared; wait for it to finish".to_owned())
                            } else {
                                fetch_reason.clone()
                            };
                            if crate::ui_ext::command_button(
                                ui,
                                egui::Button::new("Update templates"),
                                update,
                                "Download new and changed templates from the NodeCraft templates repository. Each is checked against its published digest before it is kept.",
                            )
                            .clicked()
                            {
                                ui.ctx().request_repaint();
                                action = Some(Action::UpdateTemplates);
                            }
                            if ui
                                .add_enabled(can_clear_templates, egui::Button::new("Clear downloaded"))
                                .on_hover_text("Remove downloaded templates from this computer. Bundled templates and templates in your projects stay.").on_disabled_hover_text(if template_waiting { "Templates are being updated, read or cleared; wait for it to finish" } else { "Nothing has been downloaded yet" })
                                .clicked()
                            {
                                action = Some(Action::Ask(Confirm::ClearTemplates));
                            }
                        });
                        if confirm == Some(Confirm::ClearTemplates) && can_clear_templates {
                            let question = match cached_templates {
                                1 => "Clear 1 downloaded template?".to_owned(),
                                n => format!("Clear {n} downloaded templates?"),
                            };
                            let hover = "Remove downloaded templates from this computer. Bundled templates and templates in your projects stay.";
                            confirm_shown = true;
                            action = confirm_row(ui, &question, hover, &mut confirm_focus, t)
                                .map(|remove| if remove { Action::ClearTemplates } else { Action::Keep })
                                .or(action.take());
                        }
                        ui.horizontal_wrapped(|ui| {
                            if template_waiting {
                                ui.spinner();
                            }
                            ui.label(egui::RichText::new(&template_status.0).small().color(t.text_muted));
                        });
                        if let Some(problem) = &template_status.1 {
                            ui.add(egui::Label::new(egui::RichText::new(problem).small().color(t.danger)).wrap());
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    "Bundled examples work offline. Downloaded ones are verified and kept on this computer; adding one copies it into your project.",
                                )
                                .small()
                                .color(t.text_muted),
                            )
                            .wrap(),
                        );
                        ui.add(egui::TextEdit::singleline(&mut self.library.template_query).hint_text("Find a template"));
                        let query = self.library.template_query.to_lowercase();
                        for entry in listing.templates.iter().filter(|entry| {
                            query.is_empty() || entry.title.to_lowercase().contains(&query) || entry.features.iter().any(|feature| feature.contains(&query))
                        }) {
                            let added = self.session.document().canvases().contains_key(&entry.canvas);
                            let identity = nodecraft_engine::templates::catalog::Identity::from(entry);
                            let available = listing.available(entry);
                            ui.horizontal(|ui| {
                                ui.label(format!("{} · v{}", entry.title, entry.version));
                                let label = if added {
                                    "In project"
                                } else if available {
                                    "Add template"
                                } else {
                                    "Retrieve template"
                                };
                                let enabled = !added && (available || can_fetch_templates);
                                if ui
                                    .add_enabled(enabled, egui::Button::new(label))
                                    .on_hover_text(if added {
                                        "Open this canvas from Canvases; duplicate its nodes for another copy"
                                    } else if available {
                                        "Add this exact version as a new canvas; save its assets with your project. Undo removes the addition."
                                    } else {
                                        "Retrieve and verify this exact version without adding or running it. It stays available for this project session."
                                    }).on_disabled_hover_text(if added { "Open this canvas from Canvases; duplicate its nodes for another copy" } else { fetch_reason.as_ref().err().map_or("This window can't download templates; only bundled ones can be added", String::as_str) })
                                    .clicked()
                                {
                                    ui.ctx().request_repaint();
                                    action = Some(if available { Action::AddTemplate(identity) } else { Action::RetrieveTemplate(identity) });
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
        self.library.confirm_focus = confirm_focus;
        self.library.confirm_shown = confirm_shown;
        // A question whose row wasn't drawn (another tab, the item gone, clearing no longer
        // possible) is dropped rather than left waiting.
        if !confirm_shown {
            self.library.confirm = None;
        }
        if !open {
            self.library.confirm = None;
        }
        if let Some(action) = action {
            self.library_action(action, t);
        }
        if let Some(browse) = browse {
            self.browse_action(browse, t);
        }
    }

    /// The Starter templates status line (#954): what the last update did, or that the bundled
    /// set is in use; and the first cache problem, if any.
    fn template_status(&self) -> (String, Option<String>) {
        let waiting = self.template_fetch.as_ref().is_some_and(template_fetch::TemplateFetcher::waiting);
        let line = if nodecraft_engine::templates::disk::is_clearing(&self.session) {
            "Removing downloaded templates…".to_owned()
        } else if nodecraft_engine::templates::disk::is_loading(&self.session) {
            "Reading downloaded templates…".to_owned()
        } else if waiting {
            "Updating templates…".to_owned()
        } else if self.template_fetch.is_none() || self.session.enabled("templates.update", &json!({})).is_err() {
            "Bundled templates only: this window can't download templates.".to_owned()
        } else {
            match nodecraft_engine::templates::update::last(&self.session) {
                Some(outcome) => nodecraft_engine::templates::update::notice(&outcome),
                None => "Using the bundled templates. Update to get new and changed ones.".to_owned(),
            }
        };
        let problems = nodecraft_engine::templates::disk::problems(&self.session);
        let problem = problems.first().map(|first| match problems.len() {
            1 => format!("Template cache: {first}"),
            n => format!("Template cache: {first} (and {} more)", n.saturating_sub(1)),
        });
        (line, problem)
    }

    fn list_library(&mut self) -> Result<Vec<Kept>, String> {
        let listed = self.session.dispatch("library.list", &json!({})).map_err(|error| error.message)?;
        serde_json::from_value::<Listing>(listed).map(|listing| listing.techniques).map_err(|_| "The library's list couldn't be read.".to_owned())
    }

    fn library_action(&mut self, action: Action, t: &Tokens) {
        match action {
            Action::Ask(confirm) => {
                self.library.confirm = Some(confirm);
                self.library.confirm_focus = true;
                return;
            }
            Action::Keep => {
                self.library.confirm = None;
                return;
            }
            Action::Remove(..) | Action::RemoveShader(_) | Action::ClearTemplates => self.library.confirm = None,
            _ => {}
        }
        let result = match action {
            Action::Ask(_) | Action::Keep => Ok(()),
            Action::UpdateTemplates => self.session.dispatch("templates.update", &json!({})).map(|_| ()),
            Action::ClearTemplates => self.session.dispatch("templates.clear_cache", &json!({})).map(|_| ()),
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
