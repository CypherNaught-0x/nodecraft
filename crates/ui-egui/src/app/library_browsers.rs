//! The Library's browsers (#96): the open project's assets, its past generations on the open
//! canvas, and its trained styles and Soul IDs, as tabs beside the techniques. Each lists a page
//! with `library.assets`, `library.history` or `library.styles` (read again when the document
//! or the filters change), and acts through existing commands: an asset goes on the canvas as a
//! source card with `node.add`, a result is restored with `history.select` or placed with
//! `history.extract`, and a style is wired into the selected card with `style.use`.
//!
//! Assets and results can also be ticked, up to 32 at a time across pages: "Place on canvas"
//! puts them all on the open canvas with `library.attach`, one undo step, and "Export…" saves
//! the ticked results through the workflow-result save (`export.results_plan` and
//! `export.results_start`), after a person picks the folder. "Import files…" brings files into
//! the library as source cards (`library_import`), and "Import from URL…" one file from a public
//! https address (`library_import`, over `nodecraft_engine::url_import`).
//!
//! "Save to My library" copies ticked assets and results into the library every project shares
//! (`library.save_assets`); the Assets tab's "My library" scope lists it (`library.saved`) and
//! places or removes its entries (`library.place_saved`, `library.unsave_assets`).
use super::*;
use crate::preview::Preview;
use nodecraft_engine::library_attach_cmds::MAX_ITEMS;
use nodecraft_engine::library_browse_cmds::{MAX_QUERY_BYTES, kind_of};
use nodecraft_engine::results_export::ResultSelector;
use nodecraft_types::{AssetRef, Blake3, GenerationId, MediaMeta, Mime};
use serde::Deserialize;

/// How many entries a browser shows at once.
pub(super) const PAGE: usize = 20;

/// The Library's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Tab {
    /// Techniques, shaders and starter templates.
    #[default]
    Library,
    Assets,
    History,
    Styles,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Library, Tab::Assets, Tab::History, Tab::Styles];

    fn label(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::Assets => "Assets",
            Tab::History => "History",
            Tab::Styles => "Styles",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Tab::Library => "Techniques, shaders and starter templates",
            Tab::Assets => "Every image, video, sound, 3D model and PDF in this project",
            Tab::History => "Past results on this canvas, newest first on each card",
            Tab::Styles => "Trained styles and Soul IDs on every canvas of this project",
        }
    }
}

/// Which assets the Assets tab lists: the open project's, or My library's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Scope {
    #[default]
    Project,
    Mine,
}

/// One entry of My library, as `library.saved` lists it.
#[derive(Clone, Debug, Deserialize)]
pub(super) struct SavedEntry {
    pub(super) hash: Blake3,
    pub(super) mime: Mime,
    pub(super) kind: Option<Modality>,
    pub(super) name: String,
    #[serde(default)]
    pub(super) origin: Option<String>,
    pub(super) bytes: u64,
}

/// A page of My library, with how full it is.
#[derive(Debug, Deserialize)]
pub(super) struct SavedPage {
    pub(super) assets: Vec<SavedEntry>,
    pub(super) total: usize,
    pub(super) offset: usize,
    pub(super) used_bytes: u64,
    pub(super) max_bytes: u64,
    pub(super) entries: usize,
    /// Why the index can't be read, until a person repairs it.
    #[serde(default)]
    pub(super) index_damaged: Option<String>,
    #[serde(default)]
    pub(super) damaged: Vec<String>,
}

/// One asset, as `library.assets` lists it.
#[derive(Clone, Debug, Deserialize)]
struct AssetEntry {
    hash: Blake3,
    mime: Mime,
    kind: Modality,
    #[serde(default)]
    meta: MediaMeta,
    label: String,
    canvas: CanvasId,
    canvas_title: String,
    node: NodeId,
    #[serde(default)]
    generation: Option<GenerationId>,
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    item: Option<usize>,
    uses: usize,
    #[serde(default)]
    stored: bool,
    source: Option<Source>,
}

#[derive(Clone, Debug, Deserialize)]
struct Source {
    kind: String,
    param: String,
}

/// A result's first media value, as `library.history` describes it.
#[derive(Clone, Debug, Deserialize)]
struct Shown {
    hash: Blake3,
    mime: Mime,
    #[serde(default)]
    meta: MediaMeta,
}

/// Where `history.extract` finds the value a result shows.
#[derive(Clone, Debug, Deserialize)]
struct Place {
    port: String,
    item: Option<usize>,
}

/// One past result, as `library.history` lists it.
#[derive(Clone, Debug, Deserialize)]
struct HistoryEntry {
    node: NodeId,
    label: String,
    generation: GenerationId,
    index: usize,
    count: usize,
    active: bool,
    partial: bool,
    model: Option<String>,
    media: Option<Shown>,
    text: Option<String>,
    place: Option<Place>,
}

/// One Train style card, as `library.styles` lists it.
#[derive(Clone, Debug, Deserialize)]
struct StyleEntry {
    canvas: CanvasId,
    canvas_title: String,
    id: NodeId,
    title: String,
    trainer: Option<String>,
    ready: bool,
    family: Option<String>,
    identity: bool,
    trigger: Option<String>,
    kept_by: Option<String>,
    /// For a Soul ID: listed, unlisted or unchecked against the account last fetched.
    #[serde(default)]
    available: Option<String>,
    /// Why it isn't ready: not trained yet, or its file is missing.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    #[serde(alias = "assets", alias = "generations", alias = "styles")]
    entries: Vec<T>,
    total: usize,
    offset: usize,
    /// Styles only: SOUL 2 cards in the project that name a Soul ID.
    #[serde(default)]
    soul_id_uses: usize,
}

enum Listing {
    Assets(Page<AssetEntry>),
    Saved(SavedPage),
    History(Page<HistoryEntry>),
    Styles(Page<StyleEntry>),
}

/// What a listing was read for: the tab's command and parameters, on which document revision.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    command: &'static str,
    params: String,
    project: ProjectId,
    revision: u64,
    /// The Soul ID list in use: a refresh installs a new one without a new revision.
    lists: Option<nodecraft_types::OptionListId>,
}

/// The browsers' filters and the page last listed.
#[derive(Default)]
pub(super) struct Browsers {
    pub(super) tab: Tab,
    /// The Assets tab's scope.
    pub(super) scope: Scope,
    /// Counts this window's changes to My library, so its listing is read again after each.
    pub(super) saved_stamp: u64,
    /// The address being typed for Import from URL, while its row is open.
    pub(super) url: Option<String>,
    /// The kind of asset shown; every kind when `None`.
    pub(super) kind: Option<Modality>,
    pub(super) query: String,
    /// Only the selected card's results.
    pub(super) card_only: bool,
    /// The first entry shown, per tab.
    offsets: std::collections::HashMap<Tab, usize>,
    listed: Option<(Key, Result<Listing, String>)>,
    /// Whether each style on the open canvas can go into the selected card, as `style.use`
    /// answered for this document revision and selection.
    verdicts: Option<(VerdictKey, Verdicts)>,
    /// The ticked entries, by key, for the project and canvas they were ticked on.
    picked: std::collections::BTreeMap<String, Pick>,
    picked_for: Option<(ProjectId, CanvasId)>,
    /// The document revision the ticks were last checked against.
    picked_revision: Option<u64>,
    /// The last library import's report.
    pub(super) report: Option<ImportReport>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) import: super::library_import::LibraryImport,
}

/// One ticked entry: what `library.attach` takes to place it, and what the workflow-result save
/// takes to export it (`None` when it can't be: an asset not made on the open canvas).
#[derive(Clone, Debug)]
pub(super) struct Pick {
    pub(super) tab: Tab,
    /// An entry of My library rather than of the project.
    pub(super) mine: bool,
    /// The file it holds, when it can be saved to My library.
    pub(super) save: Option<Blake3>,
    pub(super) place: Option<serde_json::Value>,
    pub(super) export: Option<ResultSelector>,
    /// The asset it is, for an Assets entry.
    pub(super) asset: Option<Blake3>,
    /// The result it is (History) or was made by (Assets), on the open canvas.
    pub(super) result: Option<(NodeId, GenerationId)>,
}

/// One file of the last library import, as its report shows it.
#[derive(Clone, Debug)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(super) struct ImportLine {
    pub(super) name: String,
    pub(super) state: String,
    pub(super) failed: bool,
}

/// What the last library import did, file by file. The paths of the failed files stay here, in
/// the window's view state, for "Retry failed"; the document never holds them.
#[derive(Clone, Debug, Default)]
pub(super) struct ImportReport {
    pub(super) summary: String,
    pub(super) lines: Vec<ImportLine>,
    pub(super) retry: Vec<std::path::PathBuf>,
}

/// What the style verdicts were checked against.
type VerdictKey = (ProjectId, u64, Option<nodecraft_types::OptionListId>, CanvasId, Option<NodeId>);
/// Per style card: the card it can go into, or why not.
type Verdicts = std::collections::HashMap<NodeId, Result<NodeId, String>>;

impl Browsers {
    fn offset(&mut self) -> &mut usize {
        let mine = self.mine();
        // My library pages on its own.
        let tab = if mine { Tab::Library } else { self.tab };
        self.offsets.entry(tab).or_default()
    }
    /// My library's page as last listed, while the tab shows it.
    pub(super) fn saved_page(&self) -> Option<&SavedPage> {
        match &self.listed {
            Some((_, Ok(Listing::Saved(page)))) if self.mine() => Some(page),
            _ => None,
        }
    }
    /// Whether the open tab shows My library.
    pub(super) fn mine(&self) -> bool {
        self.tab == Tab::Assets && self.scope == Scope::Mine
    }
    /// The ticked entries of the open tab and scope.
    fn picks(&self) -> impl Iterator<Item = &Pick> {
        let mine = self.mine();
        self.picked.values().filter(move |pick| pick.tab == self.tab && pick.mine == mine)
    }
    /// Forgets the ticks of the open tab and scope.
    pub(super) fn clear_picks(&mut self) {
        let (tab, mine) = (self.tab, self.mine());
        self.picked.retain(|_, pick| pick.tab != tab || pick.mine != mine);
    }
    /// Forgets ticks made on another project or canvas, and, whenever the document changed (a
    /// deletion, an undo), ticks of entries it no longer holds: an asset no card holds or a result
    /// that is gone is unticked; an asset whose result is gone can no longer be exported.
    fn keep_picks_for(&mut self, session: &Session, canvas: CanvasId) {
        let project = session.document().id();
        if self.picked_for != Some((project, canvas)) {
            self.picked.clear();
            self.picked_for = Some((project, canvas));
        }
        if self.picked_revision == Some(session.revision()) {
            return;
        }
        self.picked_revision = Some(session.revision());
        if self.picked.is_empty() {
            return;
        }
        let held = nodecraft_engine::library_browse_cmds::listed_hashes(session.document());
        let nodes = session.document().canvases().get(&canvas).map(|canvas| canvas.nodes());
        let made = |(node, generation): (NodeId, GenerationId)| {
            nodes.and_then(|nodes| nodes.get(&node)).is_some_and(|node| node.history.iter().any(|g| g.id == generation))
        };
        self.picked.retain(|_, pick| match pick.asset {
            // My library's entries don't follow the project.
            _ if pick.mine => true,
            Some(hash) => held.contains(&hash),
            None => pick.result.is_none_or(made),
        });
        for pick in self.picked.values_mut() {
            if pick.asset.is_some() && pick.result.is_some_and(|result| !made(result)) {
                pick.result = None;
                pick.export = None;
            }
        }
    }
}

/// What a browser asked for, applied after the panel is drawn.
#[derive(Clone, Debug)]
pub(super) enum Browse {
    /// A source card holding this asset, in the middle of the view.
    Insert {
        asset: AssetRef,
        kind: String,
        param: String,
    },
    /// Make the result the card's active one.
    Restore {
        node: NodeId,
        generation: GenerationId,
    },
    /// A source card holding the result's value, in the middle of the view.
    Extract {
        node: NodeId,
        generation: GenerationId,
        port: String,
        item: Option<usize>,
    },
    /// Wire the style into the selected card.
    UseStyle {
        style: NodeId,
        target: NodeId,
    },
    OpenCanvas(CanvasId),
    /// Every ticked entry as a source card on the open canvas, in one undo step.
    Attach(Vec<serde_json::Value>),
    /// Save the ticked results to a folder a person picks.
    Export(Vec<ResultSelector>),
    /// Pick files to import into the library.
    Import,
    /// Read the files that failed in the last import again.
    RetryImport,
    /// Fetch the file at this address and import it.
    FetchUrl(String),
    /// Copy these project files into My library.
    SaveToLibrary(Vec<Blake3>),
    /// My library's entries as source cards on the open canvas, in one undo step.
    PlaceSaved(Vec<Blake3>),
    /// Take these entries out of My library.
    Unsave(Vec<Blake3>),
    /// Rebuild My library's damaged index.
    RepairSaved,
}

/// A tick box for one entry, labelled `label`, given what is ticked now; a change goes in
/// `ticks`. At most [`MAX_ITEMS`] entries can be ticked.
pub(super) fn tick(
    ui: &mut egui::Ui,
    picked: &std::collections::BTreeMap<String, Pick>,
    key: String,
    label: &str,
    pick: Pick,
    ticks: &mut Vec<(String, Option<Pick>)>,
) {
    let mut on = picked.contains_key(&key);
    // Each tab places and exports its own ticks, so each may hold the most one request takes.
    let room = on || picked.values().filter(|p| p.tab == pick.tab && p.mine == pick.mine).count() < MAX_ITEMS;
    let response = ui.add_enabled(room, egui::Checkbox::new(&mut on, label));
    if response.on_disabled_hover_text("At most 32 entries can be ticked at once in a tab").changed() {
        ticks.push((key, on.then_some(pick)));
    }
}

/// The bar over a list with ticked entries: how many, and what can be done with them all.
fn bulk(ui: &mut egui::Ui, t: &Tokens, browsers: &mut Browsers, can_save: bool, action: &mut Option<Browse>) {
    let picks: Vec<Pick> = browsers.picks().cloned().collect();
    if picks.is_empty() {
        return;
    }
    let mut clear = false;
    // Ticking stops at the most one request takes; this holds even if that ever changes.
    let fits = picks.len() <= MAX_ITEMS;
    const TOO_MANY: &str = "At most 32 entries are handled at once; clear some ticks";
    if browsers.mine() {
        let hashes: Vec<Blake3> = picks.iter().filter_map(|pick| pick.asset).collect();
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("{} ticked", picks.len())).color(t.text));
            if ui
                .add_enabled(fits, egui::Button::new("Place on canvas"))
                .on_hover_text("Each ticked file as a source card in the middle of the view, copied into this project; one undo step")
                .on_disabled_hover_text(TOO_MANY)
                .clicked()
            {
                *action = Some(Browse::PlaceSaved(hashes.clone()));
            }
            if ui
                .add_enabled(fits, egui::Button::new("Remove from My library"))
                .on_hover_text("Take them out of My library; cards placed from them keep their own copies")
                .on_disabled_hover_text(TOO_MANY)
                .clicked()
            {
                *action = Some(Browse::Unsave(hashes.clone()));
            }
            clear = ui.button("Clear ticks").clicked();
        });
        if clear {
            browsers.clear_picks();
        }
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("{} ticked", picks.len())).color(t.text));
        let places: Option<Vec<serde_json::Value>> = picks.iter().map(|pick| pick.place.clone()).collect();
        let place = ui.add_enabled(places.is_some(), egui::Button::new("Place on canvas"));
        if place
            .on_hover_text("Each ticked entry as a source card in the middle of the view; one undo step")
            .on_disabled_hover_text(
                "A ticked entry can't become a source card: a trained style, a result made for other outputs, or a file missing from the project",
            )
            .clicked()
            && let Some(places) = places
        {
            *action = Some(Browse::Attach(places));
        }
        let exports: Option<Vec<ResultSelector>> = picks.iter().map(|pick| pick.export.clone()).collect();
        let export = ui.add_enabled(exports.is_some(), egui::Button::new("Export…"));
        if export
            .on_hover_text("Save the ticked results' original files, with their run settings, to a folder you choose")
            .on_disabled_hover_text("Only media results made on this canvas export to files; place an asset and run, or open its canvas")
            .clicked()
            && let Some(exports) = exports
        {
            *action = Some(Browse::Export(exports));
        }
        let saves: Option<Vec<Blake3>> = picks.iter().map(|pick| pick.save).collect();
        let save = ui.add_enabled(can_save && saves.is_some() && fits, egui::Button::new("Save to My library"));
        let why = if !can_save {
            "This window has no My library"
        } else if !fits {
            TOO_MANY
        } else {
            "Only images, videos, sounds, 3D models and PDFs this project holds can be saved"
        };
        if save
            .on_hover_text("Copy the ticked files into the library every project shares; only their names and this project's title go with them")
            .on_disabled_hover_text(why)
            .clicked()
            && let Some(saves) = saves
        {
            *action = Some(Browse::SaveToLibrary(saves));
        }
        clear = ui.button("Clear ticks").clicked();
    });
    if clear {
        browsers.clear_picks();
    }
}

/// The last library import's report: each file and what became of it, with Retry and Dismiss.
fn report(ui: &mut egui::Ui, t: &Tokens, browsers: &mut Browsers, action: &mut Option<Browse>) {
    let Some(report) = &browsers.report else { return };
    let mut dismiss = false;
    egui::Frame::NONE.fill(t.card_fill).corner_radius(t.panel_radius).inner_margin(t.card_padding).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("Import: {}", report.summary)).color(t.text));
        egui::ScrollArea::vertical().id_salt("library_import_report").max_height(t.card_width * 0.5).show(ui, |ui| {
            for line in &report.lines {
                let colour = if line.failed { t.danger } else { t.text_muted };
                ui.label(egui::RichText::new(format!("{}: {}", sheet_view::line(&line.name), line.state)).small().color(colour));
            }
        });
        ui.horizontal(|ui| {
            let failed = report.retry.len();
            #[cfg(not(target_arch = "wasm32"))]
            let busy = browsers.import.busy();
            #[cfg(target_arch = "wasm32")]
            let busy = true;
            if failed > 0
                && ui
                    .add_enabled(!busy, egui::Button::new(format!("Retry failed ({failed})")))
                    .on_hover_text("Read the files that failed again and import them; the rest are left alone")
                    .on_disabled_hover_text("Files are being imported; retry when they are done")
                    .clicked()
            {
                *action = Some(Browse::RetryImport);
            }
            dismiss = ui.button("Dismiss").clicked();
        });
    });
    if dismiss {
        browsers.report = None;
    }
}

/// `text` cut to at most `max` bytes at a character boundary.
pub(super) fn bounded(text: &str, max: usize) -> &str {
    let end = text.char_indices().map(|(at, c)| at.saturating_add(c.len_utf8())).take_while(|end| *end <= max).last().unwrap_or(0);
    text.get(..end).unwrap_or_default()
}

pub(super) fn kind_name(kind: Modality) -> &'static str {
    match kind {
        Modality::Image => "Image",
        Modality::Video => "Video",
        Modality::Audio => "Sound",
        Modality::Mesh => "3D model",
        Modality::Document => "PDF",
        Modality::Text => "Text",
        Modality::Style => "Style",
    }
}

/// The asset kinds the filter offers, with their labels.
const KINDS: [(Option<Modality>, &str); 6] = [
    (None, "All"),
    (Some(Modality::Image), "Images"),
    (Some(Modality::Video), "Videos"),
    (Some(Modality::Audio), "Sounds"),
    (Some(Modality::Mesh), "3D"),
    (Some(Modality::Document), "PDFs"),
];

impl NodeCraftApp {
    /// The single selected card on the open canvas, if exactly one is selected.
    pub(super) fn browse_selected_card(&self) -> Option<NodeId> {
        match self.selection.nodes().iter().collect::<Vec<_>>().as_slice() {
            [only] => Some(**only),
            _ => None,
        }
    }

    /// Lists the open tab's page again when the document or a filter changed since the last.
    pub(super) fn refresh_browser(&mut self) {
        let card = self.browse_selected_card();
        let on = self.canvas;
        let browsers = &mut self.library.browsers;
        let offset = *browsers.offset();
        let mut stamp = 0;
        let (command, params) = match browsers.tab {
            Tab::Library => return,
            Tab::Assets if browsers.scope == Scope::Mine => {
                let mut params = json!({"offset": offset, "limit": PAGE});
                if let Some(kind) = browsers.kind {
                    params["kind"] = json!(kind);
                }
                let query = bounded(browsers.query.trim(), MAX_QUERY_BYTES);
                if !query.is_empty() {
                    params["query"] = json!(query);
                }
                stamp = browsers.saved_stamp;
                ("library.saved", params)
            }
            Tab::Assets => {
                let mut params = json!({"offset": offset, "limit": PAGE});
                if let Some(kind) = browsers.kind {
                    params["kind"] = json!(kind);
                }
                let query = bounded(browsers.query.trim(), MAX_QUERY_BYTES);
                if !query.is_empty() {
                    params["query"] = json!(query);
                }
                ("library.assets", params)
            }
            Tab::History => {
                let mut params = json!({"canvas": on, "offset": offset, "limit": PAGE});
                if browsers.card_only
                    && let Some(card) = card
                {
                    params["node"] = json!(card);
                }
                ("library.history", params)
            }
            Tab::Styles => ("library.styles", json!({"offset": offset, "limit": PAGE})),
        };
        let key = Key {
            command,
            params: format!("{params}#{stamp}"),
            project: self.session.document().id(),
            revision: self.session.revision(),
            lists: nodecraft_engine::soul_ids::lists_stamp(self.session.option_lists()),
        };
        if self.library.browsers.listed.as_ref().is_some_and(|(old, _)| *old == key) {
            return;
        }
        let listed = self.session.dispatch(command, &params).map_err(|error| error.message).and_then(|value| {
            let unreadable = |_| "The list couldn't be read.".to_owned();
            match command {
                "library.assets" => serde_json::from_value(value).map(Listing::Assets).map_err(unreadable),
                "library.saved" => serde_json::from_value(value).map(Listing::Saved).map_err(unreadable),
                "library.history" => serde_json::from_value(value).map(Listing::History).map_err(unreadable),
                _ => serde_json::from_value(value).map(Listing::Styles).map_err(unreadable),
            }
        });
        // A page past the end (after a filter or a deletion) goes back to the start.
        let past_end = |page: &Result<Listing, String>| match page {
            Ok(Listing::Assets(p)) => p.entries.is_empty() && p.offset > 0,
            Ok(Listing::Saved(p)) => p.assets.is_empty() && p.offset > 0,
            Ok(Listing::History(p)) => p.entries.is_empty() && p.offset > 0,
            Ok(Listing::Styles(p)) => p.entries.is_empty() && p.offset > 0,
            Err(_) => false,
        };
        if past_end(&listed) {
            *self.library.browsers.offset() = 0;
            self.library.browsers.listed = None;
            return self.refresh_browser();
        }
        self.library.browsers.listed = Some((key, listed));
    }
}

/// The tab row.
pub(super) fn tabs(ui: &mut egui::Ui, browsers: &mut Browsers) {
    ui.horizontal(|ui| {
        for tab in Tab::ALL {
            if ui.selectable_label(browsers.tab == tab, tab.label()).on_hover_text(tab.hint()).clicked() {
                browsers.tab = tab;
            }
        }
    });
    ui.separator();
}

/// A thumbnail of `asset` at the browsers' size, or its kind's name when it has none.
fn thumbnail(ui: &mut egui::Ui, t: &Tokens, previews: &mut crate::preview::Previews, session: &Session, asset: &AssetRef, label: &str) {
    let side = t.library_thumbnail;
    let kind = kind_of(&asset.mime);
    let previewable = matches!(kind, Some(Modality::Image | Modality::Document));
    let preview = previewable.then(|| {
        let assets = session.assets();
        previews.request_asset(ui.ctx(), asset.hash, side * ui.ctx().pixels_per_point(), || assets.preview_read(asset))
    });
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    ui.painter().rect_filled(rect, t.panel_radius, t.card_fill);
    let described = match preview {
        Some(Preview::Ready { texture, size, .. }) => {
            let [w, h] = size.map(|v| v.max(1) as f32);
            let scale = (side / w).min(side / h);
            let shown = Rect::from_center_size(rect.center(), Vec2::new(w * scale, h * scale));
            egui::Image::new((texture, shown.size())).paint_at(ui, shown);
            format!("Thumbnail of {label}")
        }
        Some(Preview::Pending) => {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(t.label_size), t.text_muted);
            format!("Loading {label}")
        }
        Some(Preview::Unavailable(why)) => {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "?", egui::FontId::proportional(t.label_size), t.text_muted);
            format!("{label}: {why}")
        }
        None => {
            let name = kind.map_or("File", kind_name);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(t.label_size), t.text_muted);
            format!("{name} {label}")
        }
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &described));
}

/// "1–20 of 45" with Previous and Next; moves the page.
fn pager(ui: &mut egui::Ui, t: &Tokens, offset: &mut usize, shown: usize, total: usize) {
    if total <= PAGE && *offset == 0 {
        return;
    }
    ui.horizontal(|ui| {
        let first = offset.saturating_add(1).min(total);
        let last = offset.saturating_add(shown);
        if ui.add_enabled(*offset > 0, egui::Button::new("Previous")).clicked() {
            *offset = offset.saturating_sub(PAGE);
        }
        ui.label(egui::RichText::new(format!("{first}–{last} of {total}")).color(t.text_muted));
        if ui.add_enabled(last < total, egui::Button::new("Next")).clicked() {
            *offset = offset.saturating_add(PAGE);
        }
    });
}

/// Draws the open browser tab (not the Library one); what was asked for.
pub(super) fn show(
    ui: &mut egui::Ui,
    t: &Tokens,
    browsers: &mut Browsers,
    previews: &mut crate::preview::Previews,
    session: &Session,
    canvas: CanvasId,
    selected: Option<NodeId>,
) -> Option<Browse> {
    let mut action = None;
    let can_save = session.enabled("library.saved", &json!({})).is_ok();
    ui.set_max_width(t.dialog_width);
    browsers.keep_picks_for(session, canvas);
    match browsers.tab {
        Tab::Library => {}
        Tab::Assets => {
            ui.horizontal(|ui| {
                for (scope, label, hint) in [
                    (Scope::Project, "This project", "Every image, video, sound, 3D model and PDF in this project"),
                    (Scope::Mine, "My library", "Files you saved to use in any project"),
                ] {
                    if ui.selectable_label(browsers.scope == scope, label).on_hover_text(hint).clicked() && browsers.scope != scope {
                        browsers.scope = scope;
                        browsers.listed = None;
                    }
                }
            });
            if browsers.scope == Scope::Mine {
                super::library_saved::header(ui, t, browsers, &mut action);
            } else {
                #[cfg(not(target_arch = "wasm32"))]
                let (importing, why) = (browsers.import.busy(), "Files are being imported");
                #[cfg(target_arch = "wasm32")]
                let (importing, why) = (true, "In the browser, drop files onto the canvas to add them");
                ui.horizontal_wrapped(|ui| {
                    let import = ui.add_enabled(!importing, egui::Button::new("Import files…"));
                    if import
                        .on_hover_text(
                            "Add images, videos, sounds, 3D models and PDFs as source cards in the middle of the view; one undo step. Only each file's name is kept.",
                        )
                        .on_disabled_hover_text(why)
                        .clicked()
                    {
                        action = Some(Browse::Import);
                    }
                    super::library_saved::url_button(ui, browsers, importing);
                });
                super::library_saved::url_row(ui, t, browsers, importing, &mut action);
                report(ui, t, browsers, &mut action);
            }
            ui.horizontal_wrapped(|ui| {
                for (kind, label) in KINDS {
                    if ui.selectable_label(browsers.kind == kind, label).clicked() && browsers.kind != kind {
                        browsers.kind = kind;
                        *browsers.offset() = 0;
                    }
                }
            });
            let search = ui.add(egui::TextEdit::singleline(&mut browsers.query).hint_text("Find an asset").desired_width(t.dialog_width));
            if search.changed() {
                *browsers.offset() = 0;
            }
        }
        Tab::History => {
            // With nothing selected the whole canvas is listed, so the box isn't left ticked.
            if selected.is_none() {
                browsers.card_only = false;
            }
            let toggle = ui.add_enabled(selected.is_some(), egui::Checkbox::new(&mut browsers.card_only, "Selected card only"));
            if toggle.on_disabled_hover_text("Select one card to see only its results").changed() {
                *browsers.offset() = 0;
            }
        }
        Tab::Styles => {
            ui.label(
                egui::RichText::new(
                    "A trained style or Soul ID stays with the provider that trained it; the project keeps a handle only that account can use.",
                )
                .small()
                .color(t.text_muted),
            );
        }
    }
    if matches!(browsers.tab, Tab::Assets | Tab::History) {
        bulk(ui, t, browsers, can_save, &mut action);
    }
    let mut ticks = Vec::new();
    let mut offset = *browsers.offset();
    let key = (session.document().id(), session.revision(), nodecraft_engine::soul_ids::lists_stamp(session.option_lists()), canvas, selected);
    if browsers.verdicts.as_ref().is_none_or(|(old, _)| *old != key) {
        browsers.verdicts = Some((key, Default::default()));
    }
    let Some((_, listed)) = &browsers.listed else {
        ui.label(egui::RichText::new("Reading…").color(t.text_muted));
        return action;
    };
    let picked = &browsers.picked;
    let mut no_verdicts = Verdicts::new();
    let verdicts = browsers.verdicts.as_mut().map_or(&mut no_verdicts, |(_, verdicts)| verdicts);
    egui::ScrollArea::vertical().max_height(t.sheet_list_height).show(ui, |ui| match listed {
        Err(error) => {
            ui.colored_label(t.danger, error);
        }
        Ok(Listing::Saved(page)) => {
            super::library_saved::list(ui, t, page, picked, &mut ticks, &mut action);
            pager(ui, t, &mut offset, page.assets.len(), page.total);
        }
        Ok(Listing::Assets(page)) => {
            if page.total == 0 {
                let empty = if browsers.kind.is_some() || !browsers.query.trim().is_empty() {
                    "No asset matches."
                } else {
                    "No assets yet. Import, paste or generate media and it shows here."
                };
                ui.label(egui::RichText::new(empty).color(t.text_muted));
            }
            for entry in &page.entries {
                let asset = AssetRef { hash: entry.hash, mime: entry.mime.clone(), meta: entry.meta.clone() };
                ui.horizontal(|ui| {
                    thumbnail(ui, t, previews, session, &asset, &entry.label);
                    ui.vertical(|ui| {
                        let insertable = entry.source.is_some() && entry.stored;
                        let pick = Pick {
                            tab: Tab::Assets,
                            mine: false,
                            save: entry.stored.then_some(entry.hash),
                            place: insertable.then(|| json!({"hash": entry.hash})),
                            export: entry.generation.filter(|_| entry.canvas == canvas && entry.stored).map(|generation| ResultSelector {
                                node: entry.node,
                                generation,
                                port: entry.port.clone(),
                                item: entry.item,
                            }),
                            asset: Some(entry.hash),
                            result: entry.generation.filter(|_| entry.canvas == canvas).map(|generation| (entry.node, generation)),
                        };
                        tick(ui, picked, format!("asset:{}", entry.hash), &sheet_view::line(&entry.label), pick, &mut ticks);
                        let uses = if entry.uses > 1 { format!(" · used {}×", entry.uses) } else { String::new() };
                        ui.label(
                            egui::RichText::new(format!("{} · {}{uses}", kind_name(entry.kind), sheet_view::line(&entry.canvas_title)))
                                .small()
                                .color(t.text_muted),
                        );
                        let insert = ui.add_enabled(insertable, egui::Button::new("Insert"));
                        let why = if entry.stored { "This kind of file has no source card" } else { "The file is missing from this project" };
                        if insert.on_hover_text("A card holding it, in the middle of the view; one undo step").on_disabled_hover_text(why).clicked()
                            && let Some(source) = &entry.source
                        {
                            action = Some(Browse::Insert { asset: asset.clone(), kind: source.kind.clone(), param: source.param.clone() });
                        }
                    });
                });
            }
            pager(ui, t, &mut offset, page.entries.len(), page.total);
        }
        Ok(Listing::History(page)) => {
            if page.total == 0 {
                ui.label(egui::RichText::new("No results on this canvas yet. Run a card and its results show here.").color(t.text_muted));
            }
            for entry in &page.entries {
                ui.horizontal(|ui| {
                    let title = format!("{} · {}/{}", sheet_view::line(&entry.label), entry.index, entry.count);
                    if let Some(media) = &entry.media {
                        let asset = AssetRef { hash: media.hash, mime: media.mime.clone(), meta: media.meta.clone() };
                        thumbnail(ui, t, previews, session, &asset, &title);
                    }
                    ui.vertical(|ui| {
                        let pick = Pick {
                            tab: Tab::History,
                            mine: false,
                            save: entry.media.as_ref().map(|media| media.hash),
                            place: entry.place.as_ref().map(|place| {
                                json!({"from": {"canvas": canvas, "node": entry.node, "generation": entry.generation, "port": place.port, "item": place.item}})
                            }),
                            export: entry.place.as_ref().filter(|_| entry.media.is_some()).map(|place| ResultSelector {
                                node: entry.node,
                                generation: entry.generation,
                                port: Some(place.port.clone()),
                                item: place.item,
                            }),
                            asset: None,
                            result: Some((entry.node, entry.generation)),
                        };
                        tick(ui, picked, format!("result:{}:{}", entry.node, entry.generation), &title, pick, &mut ticks);
                        let mut facts: Vec<String> = entry.model.iter().cloned().collect();
                        if entry.active {
                            facts.push("in use".into());
                        }
                        if entry.partial {
                            facts.push("some items missing".into());
                        }
                        if !facts.is_empty() {
                            ui.label(egui::RichText::new(facts.join(" · ")).small().color(t.text_muted));
                        }
                        if let Some(text) = &entry.text {
                            ui.label(egui::RichText::new(sheet_view::line(text)).small().color(t.text_muted));
                        }
                        ui.horizontal(|ui| {
                            let restore = ui.add_enabled(!entry.active, egui::Button::new("Use this result"));
                            if restore
                                .on_hover_text("Make it the card's active result, the one downstream cards use; one undo step")
                                .on_disabled_hover_text("The card uses this result now")
                                .clicked()
                            {
                                action = Some(Browse::Restore { node: entry.node, generation: entry.generation });
                            }
                            let place = ui.add_enabled(entry.place.is_some(), egui::Button::new("Place"));
                            if place
                                .on_hover_text("A source card holding it, in the middle of the view, feeding cards at once; one undo step")
                                .on_disabled_hover_text(
                                    "This result can't become a source card: it is a trained style, or was made for other outputs than the card has now",
                                )
                                .clicked()
                                && let Some(place) = &entry.place
                            {
                                action = Some(Browse::Extract { node: entry.node, generation: entry.generation, port: place.port.clone(), item: place.item });
                            }
                        });
                    });
                });
            }
            pager(ui, t, &mut offset, page.entries.len(), page.total);
        }
        Ok(Listing::Styles(page)) => {
            if page.total == 0 {
                ui.label(egui::RichText::new("No trained styles yet. Add a Train style card and run it.").color(t.text_muted));
            }
            if page.soul_id_uses > 0 {
                let cards = if page.soul_id_uses == 1 { "1 SOUL 2 card names".to_owned() } else { format!("{} SOUL 2 cards name", page.soul_id_uses) };
                ui.label(egui::RichText::new(format!("{cards} a Soul ID: it runs only on the Higgsfield account that trained it")).small().color(t.text_muted));
            }
            for entry in &page.entries {
                let name = if !entry.title.trim().is_empty() { entry.title.as_str() } else { entry.trainer.as_deref().unwrap_or("Train style") };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(sheet_view::line(name)).color(t.text));
                    if entry.identity {
                        ui.label(egui::RichText::new("Soul ID").small().color(t.accent)).on_hover_text("A trained identity, picked on SOUL 2 cards");
                    }
                });
                let mut facts = vec![sheet_view::line(&entry.canvas_title)];
                facts.extend(entry.family.iter().map(|family| format!("for {family}")));
                facts.extend(entry.trigger.iter().filter(|t| !t.trim().is_empty()).map(|trigger| format!("trigger “{}”", sheet_view::line(trigger))));
                if !entry.ready {
                    facts.push(entry.reason.as_deref().map_or_else(|| "not trained yet".to_owned(), sheet_view::line));
                }
                ui.label(egui::RichText::new(facts.join(" · ")).small().color(t.text_muted));
                if entry.identity {
                    // A handle to one account's identity, never a file to hand on (#180).
                    ui.label(
                        egui::RichText::new("Bound to the Higgsfield account that trained it: not a portable file, and not exported")
                            .small()
                            .color(t.text_muted),
                    );
                    match entry.available.as_deref() {
                        Some("unlisted") => {
                            ui.label(
                                egui::RichText::new(
                                    "Unavailable on this account: cards keep it, but runs are refused until you use the account that trained it",
                                )
                                .small()
                                .color(t.danger),
                            );
                        }
                        Some("unreadable") => {
                            ui.label(egui::RichText::new("Its descriptor is missing or doesn't name a Soul ID; train it again").small().color(t.danger));
                        }
                        Some("unchecked") => {
                            ui.label(egui::RichText::new("Not checked on this account yet; Refresh option lists checks it").small().color(t.text_muted));
                        }
                        _ => {}
                    }
                } else if let Some(provider) = &entry.kept_by {
                    ui.label(egui::RichText::new(format!("Kept by {}: only that account can use it", sheet_view::line(provider))).small().color(t.text_muted));
                }
                if entry.canvas == canvas {
                    let verdict = verdicts
                        .entry(entry.id)
                        .or_insert_with(|| {
                            let edge = ids::edge_id(session.document(), ("library-style", session.revision(), entry.id));
                            match (selected, edge) {
                                (Some(target), Some(edge)) => session
                                    .enabled("style.use", &json!({"canvas": canvas, "style": entry.id, "target": target, "edge": edge}))
                                    .map(|()| target)
                                    .map_err(|error| error.message),
                                (None, _) => Err("Select the card to style".to_owned()),
                                (_, None) => Err("Couldn't allocate an edge id; try again".to_owned()),
                            }
                        })
                        .clone();
                    let button = ui.add_enabled(verdict.is_ok(), egui::Button::new("Use on selected card"));
                    let button = match &verdict {
                        Ok(_) if entry.identity => button.on_hover_text("Set the selected SOUL 2 card's Soul ID to this one; one undo step. It doesn't run."),
                        Ok(_) => button.on_hover_text("Wire this style into the selected card's style input; one undo step. It doesn't run."),
                        Err(why) => button.on_disabled_hover_text(why.as_str()),
                    };
                    if button.clicked()
                        && let Ok(target) = verdict
                    {
                        action = Some(Browse::UseStyle { style: entry.id, target });
                    }
                } else if ui.button("Open its canvas").on_hover_text("Go to the canvas holding this Train style card").clicked() {
                    action = Some(Browse::OpenCanvas(entry.canvas));
                }
                ui.separator();
            }
            pager(ui, t, &mut offset, page.entries.len(), page.total);
        }
    });
    *browsers.offset() = offset;
    for (key, pick) in ticks {
        match pick {
            Some(pick) => browsers.picked.insert(key, pick),
            None => browsers.picked.remove(&key),
        };
    }
    action
}

impl NodeCraftApp {
    /// Applies what a browser asked for.
    pub(super) fn browse_action(&mut self, action: Browse, t: &Tokens) {
        let result = match action {
            Browse::Insert { asset, kind, param } => self.browse_insert_asset(t, asset, &kind, &param),
            Browse::Restore { node, generation } => {
                self.session.dispatch("history.select", &json!({"canvas": self.canvas, "id": node, "generation": generation})).map(|_| ())
            }
            Browse::Extract { node, generation, port, item } => self.browse_extract(t, node, generation, port, item),
            Browse::UseStyle { style, target } => {
                self.use_style(target, style);
                return;
            }
            Browse::OpenCanvas(canvas) => {
                self.open_canvas(canvas);
                Ok(())
            }
            Browse::Attach(items) => self.browse_attach(t, items),
            Browse::Export(targets) => {
                self.start_results_save(Some(targets));
                return;
            }
            Browse::Import => {
                #[cfg(not(target_arch = "wasm32"))]
                self.pick_library_files();
                return;
            }
            Browse::FetchUrl(address) => {
                #[cfg(not(target_arch = "wasm32"))]
                self.fetch_library_url(address);
                #[cfg(target_arch = "wasm32")]
                {
                    drop(address);
                    self.notice = Some(super::library_saved::NO_URL_IMPORT.into());
                }
                return;
            }
            // These say what they did in the notice themselves.
            Browse::SaveToLibrary(hashes) => {
                if let Err(error) = self.save_to_library(hashes) {
                    self.notice = Some(error.message);
                }
                return;
            }
            Browse::Unsave(hashes) => {
                if let Err(error) = self.unsave(hashes) {
                    self.notice = Some(error.message);
                }
                return;
            }
            Browse::RepairSaved => {
                if let Err(error) = self.repair_saved() {
                    self.notice = Some(error.message);
                }
                return;
            }
            Browse::PlaceSaved(hashes) => self.place_saved(t, hashes),
            Browse::RetryImport => {
                // The paths are given up only once the reading has started, so a refusal keeps them.
                let retry = self.library.browsers.report.as_ref().map(|report| report.retry.clone()).unwrap_or_default();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    if self.read_library_files(retry)
                        && let Some(report) = self.library.browsers.report.as_mut()
                    {
                        report.retry.clear();
                    }
                }
                #[cfg(target_arch = "wasm32")]
                drop(retry);
                return;
            }
        };
        match result {
            Ok(()) => self.notice = None,
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Every ticked entry as a source card with `library.attach`, in a grid centred in the view,
    /// selected; the ticks of the open tab are cleared.
    fn browse_attach(&mut self, t: &Tokens, items: Vec<serde_json::Value>) -> Result<(), Error> {
        let centre = self.browse_centre(t)?;
        let mut fresh: Vec<NodeId> = Vec::with_capacity(items.len());
        for index in 0..items.len() {
            let id = ids::node_id(self.session.document(), ("library-attach", self.session.revision(), index))
                .filter(|id| !fresh.contains(id))
                .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate node ids; try again."))?;
            fresh.push(id);
        }
        let step = t.card_width + t.card_padding * 4.0;
        let columns = nodecraft_engine::library_attach_cmds::columns(items.len());
        let rows = items.len().div_ceil(columns);
        let at = Pos2::new(centre.x - step * columns as f32 * 0.5, centre.y - step * rows as f32 * 0.5);
        let params = json!({"canvas": self.canvas, "items": items, "ids": fresh, "at": {"x": at.x, "y": at.y}, "step": step});
        self.session.dispatch("library.attach", &params)?;
        self.selection.replace(fresh.into_iter().collect());
        self.library.browsers.clear_picks();
        Ok(())
    }

    /// The middle of the usable view, in canvas coordinates.
    pub(super) fn browse_centre(&self, t: &Tokens) -> Result<Pos2, Error> {
        let usable = self.navigation.usable(self.viewport, t);
        let centre = self.view.to_world(self.viewport.min, usable.center());
        if !canvas::valid_rect(usable) || !centre.is_finite() {
            return Err(Error::new(ErrorKind::Conflict, "There is no canvas space to place it in."));
        }
        Ok(centre)
    }

    /// A source card holding `asset`, centred in the view and selected, with `node.add`.
    fn browse_insert_asset(&mut self, t: &Tokens, asset: AssetRef, kind: &str, param: &str) -> Result<(), Error> {
        let centre = self.browse_centre(t)?;
        let id = ids::node_id(self.session.document(), ("library-asset", self.session.revision(), asset.hash))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate a node id; try again."))?;
        let supplied: nodecraft_types::ParamValues = [(param.to_owned(), Value::Media(asset))].into_iter().collect();
        let mut node = self.session.kinds().get(kind)?.instantiate_instance(id, BTreeMap::new(), &supplied)?;
        let size = payload_rect(&self.session, &node, t).size();
        node.pos = nodecraft_graph::Position { x: centre.x - size.x * 0.5, y: centre.y - size.y * 0.5 };
        self.session.dispatch("node.add", &json!({"canvas": self.canvas, "node": node}))?;
        self.selection.only(id);
        Ok(())
    }

    /// `history.extract` of one result's value, centred in the view and selected.
    fn browse_extract(&mut self, t: &Tokens, node: NodeId, generation: GenerationId, port: String, item: Option<usize>) -> Result<(), Error> {
        let centre = self.browse_centre(t)?;
        let id = ids::node_id(self.session.document(), ("library-extract", self.session.revision(), node, generation))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "Couldn't allocate a node id; try again."))?;
        let mut params = json!({"canvas": self.canvas, "id": node, "generation": generation, "port": port, "node": id, "pos": {"x": centre.x, "y": centre.y}});
        if let Some(item) = item {
            params["item"] = json!(item);
        }
        self.centered_command("history.extract", &mut params, id)?;
        self.selection.only(id);
        Ok(())
    }
}
