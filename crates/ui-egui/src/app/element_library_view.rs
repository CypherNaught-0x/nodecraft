//! The Elements library (#552) from the canvas: an Element card's menu saves it to the library
//! or updates it from there, and the Elements panel places a saved Element in this project,
//! renames, shares, exports or removes it, and imports one someone sent (#765). The list is read
//! when the panel opens and after every change.
use super::*;
use nodecraft_engine::element_bundle::BUNDLE_EXTENSION;
use nodecraft_engine::element_library_cmds::ENTRY_CONFIG;
use serde::Deserialize;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll, Wake, Waker};

type Pick = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

/// The Elements panel's state: whether it is open and the library as last listed.
#[derive(Default)]
pub(super) struct ElementLibraryView {
    pub(super) open: bool,
    /// The last `library.elements` answer; `None` reads it again when next needed.
    listed: Option<Result<Listing, String>>,
    /// The entry being renamed and the name typed so far.
    renaming: Option<(String, String)>,
    /// The rename field takes the keyboard when next drawn.
    focus_rename: bool,
    /// The entry whose Remove asks to be confirmed.
    /// The entry asked to be removed, and whether it is a damaged one.
    removing: Option<(String, bool)>,
    /// A bundle to import is being chosen.
    importing: Option<Pick>,
    /// A chosen bundle whose Element's name the library has already: replace it, or not.
    clashing: Option<PathBuf>,
    /// What the test file dialog answers.
    #[cfg(test)]
    pub(super) answer: Option<PathBuf>,
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

/// An entry in the library file this build can't use, as `library.elements` reports it.
#[derive(Clone, Debug, Deserialize)]
struct Damaged {
    index: usize,
    name: Option<String>,
    reason: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Listing {
    elements: Vec<Listed>,
    #[serde(default)]
    damaged: Vec<Damaged>,
    #[serde(default)]
    file: Option<String>,
}

/// What an Element card's menu asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    /// Keep the Element in the library, over an entry of its name when `replace`.
    Save { id: NodeId, replace: bool },
    /// Bring it up to date with the library's entry it was placed from (or of its name).
    Update(NodeId),
}

/// What the panel asked for, applied after it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Place(String),
    StartRename(String),
    /// The name typed so far for the entry being renamed.
    Draft(String),
    Rename {
        name: String,
        to: String,
    },
    StopRename,
    Share {
        name: String,
        shared: bool,
    },
    Export(String),
    AskRemove {
        name: String,
        damaged: bool,
    },
    Remove {
        name: String,
        damaged: bool,
    },
    KeepIt,
    Import,
    ImportReplacing(PathBuf),
    CancelImport,
}

impl ElementLibraryView {
    /// Whether the library (as last listed) has an entry called `name`.
    fn has(&self, name: &str) -> bool {
        matches!(&self.listed, Some(Ok(listing)) if listing.elements.iter().any(|l| l.name.eq_ignore_ascii_case(name)))
    }

    /// The entry `node` updates from: the one it was placed from, else the one of its name.
    pub(super) fn source(&self, node: &Node) -> Option<String> {
        // As `element.update` picks: its own name first, then the entry it was placed from.
        let placed_from = match node.config.get(ENTRY_CONFIG) {
            Some(Value::Text(entry)) => Some(entry.as_str()),
            _ => None,
        };
        nodecraft_nodes::element::name(node).into_iter().chain(placed_from).find(|name| self.has(name)).map(str::to_owned)
    }
}

/// The rename field's id, so starting a rename can focus it.
fn rename_id() -> egui::Id {
    egui::Id::new("nodecraft.element_library.rename")
}

struct Repaint(egui::Context);
impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.request_repaint();
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
    if let Some(source) = view.source(node)
        && ui.button("Update from Elements library").on_hover_text(format!("Take the images, category and guidelines of the library's {source}")).clicked()
    {
        action = Some(CardAction::Update(node.id));
        ui.close();
    }
    action
}

impl NodeCraftApp {
    pub(super) fn toggle_elements(&mut self) {
        self.element_library.open = !self.element_library.open;
        self.element_library.listed = None;
    }

    /// Whether this window keeps an Elements library.
    fn has_element_library(&self) -> bool {
        self.session.enabled("library.elements", &json!({})).is_ok()
    }

    /// Reads the library's list when it isn't at hand, so Element card menus know what it holds.
    pub(super) fn element_library_listed(&mut self) -> &ElementLibraryView {
        if self.element_library.listed.is_none() && self.has_element_library() {
            let listed = self
                .session
                .dispatch("library.elements", &json!({}))
                .map_err(|error| error.message)
                .and_then(|listed| serde_json::from_value::<Listing>(listed).map_err(|_| "The Elements library's list couldn't be read.".to_owned()));
            self.element_library.listed = Some(listed);
        }
        &self.element_library
    }

    /// The panel listing the library's Elements.
    pub(super) fn elements_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        self.poll_element_import(ctx);
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
                let view = &self.element_library;
                if let Some(path) = &view.clashing {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("The library already has an Element with that bundle's name.");
                        if ui.button("Replace it").on_hover_text("Import the bundle over the library's Element of that name").clicked() {
                            action = Some(Action::ImportReplacing(path.clone()));
                        }
                        if ui.button("Cancel").on_hover_text("Rename the library's Element first to keep both").clicked() {
                            action = Some(Action::CancelImport);
                        }
                    });
                    ui.separator();
                }
                egui::ScrollArea::vertical().max_height(t.sheet_list_height).show(ui, |ui| match &view.listed {
                    Some(Err(error)) => {
                        ui.colored_label(t.danger, error);
                    }
                    Some(Ok(listing)) => {
                        if listing.elements.is_empty() {
                            ui.label(
                                egui::RichText::new("The library is empty. Save an Element from its card's menu to use it in other projects.")
                                    .color(t.text_muted),
                            );
                        }
                        for entry in &listing.elements {
                            ui.horizontal(|ui| {
                                if let Some((_, draft)) = view.renaming.as_ref().filter(|(name, _)| *name == entry.name) {
                                    let mut draft = draft.clone();
                                    let edit = egui::TextEdit::singleline(&mut draft)
                                        .id(rename_id())
                                        .desired_width(t.card_width / 2.0)
                                        .char_limit(nodecraft_nodes::element::MAX_NAME);
                                    let edit = ui.add(edit).on_hover_text("Enter renames it; Escape or clicking away keeps its name");
                                    if view.focus_rename {
                                        edit.request_focus();
                                    }
                                    if edit.lost_focus() {
                                        // Enter renames; Escape or a click elsewhere keeps the name.
                                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                        action = Some(if enter { Action::Rename { name: entry.name.clone(), to: draft } } else { Action::StopRename });
                                    } else if edit.changed() {
                                        action = Some(Action::Draft(draft));
                                    }
                                    return;
                                }
                                let images = if entry.images == 1 { "1 image".to_owned() } else { format!("{} images", entry.images) };
                                let sheet = if entry.sheet { " + sheet" } else { "" };
                                ui.label(format!("#{} · {} · {images}{sheet}", sheet_view::line(&entry.name), entry.category));
                                if view.removing.as_ref().is_some_and(|(name, damaged)| !damaged && *name == entry.name) {
                                    if ui.button("Yes, remove").on_hover_text("Projects that placed it keep their copy").clicked() {
                                        action = Some(Action::Remove { name: entry.name.clone(), damaged: false });
                                    }
                                    if ui.button("Keep").clicked() {
                                        action = Some(Action::KeepIt);
                                    }
                                    return;
                                }
                                if ui.button("Place").on_hover_text("Copy it into this project, in the middle of the view").clicked() {
                                    action = Some(Action::Place(entry.name.clone()));
                                }
                                let mut shared = entry.shared;
                                if ui.checkbox(&mut shared, "Shared").on_hover_text("Mark it meant for others; Elements are private until you do").changed() {
                                    action = Some(Action::Share { name: entry.name.clone(), shared });
                                }
                                ui.menu_button("…", |ui| {
                                    if ui
                                        .button("Rename")
                                        .on_hover_text("Elements already on a canvas update from it only if they carry its new name")
                                        .clicked()
                                    {
                                        action = Some(Action::StartRename(entry.name.clone()));
                                        ui.close();
                                    }
                                    if ui
                                        .button("Export…")
                                        .on_hover_text("Save it as a file someone else imports: its settings and images, nothing else")
                                        .clicked()
                                    {
                                        action = Some(Action::Export(entry.name.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Remove").on_hover_text("Take it out of the library; projects that placed it keep their copy").clicked() {
                                        action = Some(Action::AskRemove { name: entry.name.clone(), damaged: false });
                                        ui.close();
                                    }
                                });
                            });
                        }
                        if !listing.damaged.is_empty() {
                            ui.separator();
                            let count = listing.damaged.len();
                            let file = listing.file.as_deref().map(|file| format!(" in {file}")).unwrap_or_default();
                            ui.colored_label(
                                t.danger,
                                format!(
                                    "{count} {} can't be used; kept as {}{file}:",
                                    if count == 1 { "entry" } else { "entries" },
                                    if count == 1 { "it is" } else { "they are" }
                                ),
                            );
                            for damaged in &listing.damaged {
                                let name = damaged
                                    .name
                                    .as_deref()
                                    .map_or_else(|| format!("entry {}", damaged.index.saturating_add(1)), |name| format!("#{}", sheet_view::line(name)));
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("{name}: {}", damaged.reason)).color(t.text_muted));
                                    // One with a name can be taken out here; the rest only in the file.
                                    let Some(name) = &damaged.name else { return };
                                    if view.removing.as_ref().is_some_and(|(asked, damaged)| *damaged && asked == name) {
                                        if ui.button("Yes, remove").on_hover_text("Its images go too unless another entry shows them").clicked() {
                                            action = Some(Action::Remove { name: name.clone(), damaged: true });
                                        }
                                        if ui.button("Keep").clicked() {
                                            action = Some(Action::KeepIt);
                                        }
                                    } else if ui.button("Remove").on_hover_text("Take this entry out of the library file").clicked() {
                                        action = Some(Action::AskRemove { name: name.clone(), damaged: true });
                                    }
                                });
                            }
                        }
                    }
                    None => {}
                });
                ui.separator();
                if ui.button("Import…").on_hover_text("Add an Element someone sent you (.ncelement)").clicked() {
                    action = Some(Action::Import);
                }
            });
        self.element_library.open = open;
        self.element_library.focus_rename = false;
        if let Some(action) = action {
            let view = &mut self.element_library;
            let result = match action {
                Action::Place(name) => self.place_element(t, &name),
                Action::StartRename(name) => {
                    view.renaming = Some((name.clone(), name));
                    view.removing = None;
                    view.focus_rename = true;
                    return;
                }
                Action::Draft(draft) => {
                    if let Some((_, typed)) = &mut view.renaming {
                        *typed = draft;
                    }
                    return;
                }
                Action::StopRename => {
                    view.renaming = None;
                    return;
                }
                Action::Rename { name, to } => {
                    if name == to {
                        view.renaming = None;
                        return;
                    }
                    let renamed = self.session.dispatch("library.rename_element", &json!({"name": name, "to": to})).map(|_| ());
                    // A refused name stays in the field to correct.
                    if renamed.is_ok() {
                        self.element_library.renaming = None;
                    } else {
                        self.element_library.focus_rename = true;
                    }
                    renamed
                }
                Action::Share { name, shared } => self.session.dispatch("library.share_element", &json!({"name": name, "shared": shared})).map(|_| ()),
                Action::Export(name) => {
                    let file = format!("{name}.{BUNDLE_EXTENSION}");
                    self.pick_export(export::Target::File("library.export_element", json!({"name": name})), &file);
                    return;
                }
                Action::AskRemove { name, damaged } => {
                    view.removing = Some((name, damaged));
                    view.renaming = None;
                    return;
                }
                Action::KeepIt => {
                    view.removing = None;
                    return;
                }
                Action::Remove { name, damaged } => {
                    view.removing = None;
                    self.session.dispatch("library.remove_element", &json!({"name": name, "damaged": damaged})).map(|_| ())
                }
                Action::Import => {
                    view.clashing = None;
                    self.start_element_import();
                    return;
                }
                Action::ImportReplacing(path) => {
                    view.clashing = None;
                    self.session.dispatch("library.import_element", &json!({"path": path, "replace": true})).map(|_| ())
                }
                Action::CancelImport => {
                    view.clashing = None;
                    return;
                }
            };
            self.element_library_done(result);
        }
    }

    fn start_element_import(&mut self) {
        if self.element_library.importing.is_some() {
            return;
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.element_import_pick())) {
            Ok(Some(pick)) => self.element_library.importing = Some(pick),
            Ok(None) => self.notice = Some("Importing an Element works in the desktop app.".into()),
            Err(_) => self.notice = Some("The file picker failed; nothing was imported.".into()),
        }
    }

    #[cfg(all(not(test), not(target_arch = "wasm32")))]
    fn element_import_pick(&self) -> Option<Pick> {
        // Built on the UI thread: AppKit finds its window here.
        let future = rfd::AsyncFileDialog::new().set_title("Import an Element").add_filter("Element", &[BUNDLE_EXTENSION]).pick_file();
        Some(Box::pin(async move { future.await.map(|file| file.path().to_path_buf()) }))
    }
    #[cfg(all(not(test), target_arch = "wasm32"))]
    fn element_import_pick(&self) -> Option<Pick> {
        None
    }
    #[cfg(test)]
    fn element_import_pick(&self) -> Option<Pick> {
        Some(Box::pin(std::future::ready(self.element_library.answer.clone())))
    }

    /// Moves an open import dialog along; a chosen bundle is imported at once.
    fn poll_element_import(&mut self, ctx: &egui::Context) {
        let Some(mut pick) = self.element_library.importing.take() else {
            return;
        };
        let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
        let mut cx = TaskContext::from_waker(&waker);
        let path = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pick.as_mut().poll(&mut cx))) {
            Ok(Poll::Pending) => {
                self.element_library.importing = Some(pick);
                return;
            }
            Ok(Poll::Ready(None)) => return,
            Ok(Poll::Ready(Some(path))) => path,
            Err(_) => {
                self.notice = Some("The file picker failed; nothing was imported.".into());
                return;
            }
        };
        let result = self.session.dispatch("library.import_element", &json!({"path": path}));
        match result {
            Err(error) if error.kind == ErrorKind::Conflict => {
                self.element_library.open = true;
                self.element_library.clashing = Some(path);
                self.element_library.listed = None;
            }
            result => self.element_library_done(result.map(|_| ())),
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
        let mut params = json!({"canvas": self.canvas, "name": name, "id": id, "pos": {"x": centre.x, "y": centre.y}});
        if free != name {
            params["as"] = json!(free);
        }
        self.centered_command("element.place", &mut params, id)?;
        self.selection.only(id);
        Ok(())
    }
}
