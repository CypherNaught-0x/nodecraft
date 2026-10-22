//! The shell owns view state; the engine owns all document data and mutations.
use crate::{
    canvas::{self, CanvasView},
    card::{self, EditTarget, EditorState, Intent},
    clipboard,
    history::HistoryIntent,
    ids,
    navigation::Navigation,
    noodle::{self, Side},
    palette::{self, Palette},
    selection::{self, Selection},
    theme::{self, ThemeKind, Tokens},
    turn_into::{self, TurnInto},
};
use egui::{Pos2, Rect, Sense, Vec2};
use nodecraft_engine::{
    Session,
    settings::{CanvasSettings, NodeCraftSettings, Theme},
};
use nodecraft_graph::{Canvas, Change, Endpoint, Node, Project};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, FieldId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::json;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

#[cfg(test)]
thread_local! { static PASTE_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub(crate) const MAX_SCHEMA_CACHE_BYTES: usize = 16 * 1024 * 1024;
struct CachedSchema {
    descriptor: Arc<nodecraft_nodes::NodeDescriptor>,
    visible: BTreeSet<String>,
    enabled: Option<BTreeSet<String>>,
    roles: card::Roles,
    listed: BTreeMap<String, card::Listed>,
    catalog: Option<nodecraft_engine::generation_catalog::GenerationCatalogStamp>,
    descriptor_bytes: usize,
    borrowed_identity: Option<usize>,
    revision: u64,
    last_used: u64,
}

/// Resolved instance schemas of the cards on screen, with the byte total kept up to date on every
/// insert and remove (shared descriptors counted once). Tree updates are O(log n); byte charges
/// visit visibility names, schema resolution depends on descriptor size, and each eviction scans
/// entries for the oldest.
/// Edits invalidate entries by document revision.
#[derive(Default)]
struct SchemaCache {
    entries: BTreeMap<NodeId, CachedSchema>,
    /// Borrowed descriptors shared between entries: (descriptor, bytes, entries using it).
    shared: BTreeMap<usize, (Arc<nodecraft_nodes::NodeDescriptor>, usize, usize)>,
    bytes: usize,
}

impl SchemaCache {
    /// Bytes an entry adds on its own (overhead, visibility names, an owned descriptor).
    fn own_bytes(entry: &CachedSchema) -> usize {
        let names = entry.visible.iter().chain(entry.enabled.iter().flatten()).map(|name| name.len().saturating_add(96)).fold(0usize, usize::saturating_add);
        let owned = if entry.borrowed_identity.is_none() { entry.descriptor_bytes } else { 0 };
        let listed = entry.listed.iter().map(|(name, listed)| name.len().saturating_add(listed.bytes())).fold(0usize, usize::saturating_add);
        256usize.saturating_add(names).saturating_add(owned).saturating_add(entry.roles.bytes()).saturating_add(listed)
    }
    /// Drops the entries drawing a run-time option list, after a fetch changed the lists
    /// (#676); the document revision alone doesn't see that.
    fn forget_listed(&mut self) {
        let listed: Vec<_> = self.entries.iter().filter(|(_, entry)| !entry.listed.is_empty()).map(|(id, _)| *id).collect();
        for id in listed {
            self.remove(&id);
        }
    }
    // Before geometry reads, invalidate only generation entries whose runtime resource/
    // definition binding changed. No document hashing or per-node payload walk in paint.
    fn refresh_generation(&mut self, session: &Session) {
        let current = session.generation_catalog().ok().and_then(|catalog| {
            // Every generation kind is installed from one snapshot, so the image kind (always
            // present) stands for all of them.
            catalog.validate_definition_binding(session.kinds().get(nodecraft_nodes::generation::KIND).ok()?).ok()?;
            Some(catalog.stamp())
        });
        let gone: Vec<_> = self.entries.iter().filter(|(_, entry)| entry.catalog.is_some() && entry.catalog != current).map(|(id, _)| *id).collect();
        for id in gone {
            self.remove(&id);
        }
    }
    fn get(&self, id: &NodeId) -> Option<&CachedSchema> {
        self.entries.get(id)
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    fn values(&self) -> impl Iterator<Item = &CachedSchema> {
        self.entries.values()
    }
    #[cfg(test)]
    fn clear(&mut self) {
        *self = Self::default();
    }
    fn insert(&mut self, id: NodeId, entry: CachedSchema) {
        self.remove(&id);
        self.bytes = self.bytes.saturating_add(Self::own_bytes(&entry));
        if let Some(identity) = entry.borrowed_identity {
            let slot = self.shared.entry(identity).or_insert_with(|| (Arc::clone(&entry.descriptor), entry.descriptor_bytes, 0));
            if slot.2 == 0 {
                self.bytes = self.bytes.saturating_add(slot.1);
            }
            slot.2 = slot.2.saturating_add(1);
        }
        self.entries.insert(id, entry);
    }
    fn remove(&mut self, id: &NodeId) {
        let Some(entry) = self.entries.remove(id) else { return };
        self.bytes = self.bytes.saturating_sub(Self::own_bytes(&entry));
        if let Some(identity) = entry.borrowed_identity
            && let Some(slot) = self.shared.get_mut(&identity)
        {
            slot.2 = slot.2.saturating_sub(1);
            if slot.2 == 0 {
                self.bytes = self.bytes.saturating_sub(slot.1);
                self.shared.remove(&identity);
            }
        }
    }
    fn retain(&mut self, keep: impl Fn(&NodeId) -> bool) {
        let gone: Vec<NodeId> = self.entries.keys().filter(|id| !keep(id)).copied().collect();
        for id in gone {
            self.remove(&id);
        }
    }
}

/// The cache's byte total, recomputed from scratch (tests check the running total against it).
#[cfg(test)]
fn cache_bytes(cache: &SchemaCache) -> usize {
    let mut shared = BTreeSet::new();
    let total = cache.entries.values().fold(0usize, |total, entry| {
        let descriptor = match entry.borrowed_identity {
            Some(identity) if !shared.insert(identity) => 0,
            Some(_) => entry.descriptor_bytes,
            None => 0,
        };
        total.saturating_add(SchemaCache::own_bytes(entry)).saturating_add(descriptor)
    });
    assert_eq!(total, cache.bytes, "running schema-cache total drifted");
    total
}
pub(crate) fn descriptor_bytes(descriptor: &nodecraft_nodes::NodeDescriptor) -> Option<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_SCHEMA_CACHE_BYTES {
                return Err(std::io::Error::other("view schema budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, descriptor).ok()?;
    // Conservative heap accounting for Vec/String/enum/tree overhead beyond wire text.
    counter.0.checked_mul(4).filter(|bytes| *bytes <= MAX_SCHEMA_CACHE_BYTES)
}
fn cached_schema<'a>(cache: &'a mut SchemaCache, session: &Session, node: &nodecraft_graph::Node, clock: u64) -> Option<&'a CachedSchema> {
    let kind = session.kinds().get(&node.kind).ok()?;
    let catalog = if nodecraft_nodes::generation::is_generation(&node.kind) {
        let binding = session.generation_catalog().and_then(|catalog| {
            catalog.validate_definition_binding(kind)?;
            Ok(catalog.stamp())
        });
        match binding {
            Ok(stamp) => Some(stamp),
            Err(_) => {
                cache.remove(&node.id);
                return None;
            }
        }
    } else {
        None
    };
    if cache.get(&node.id).is_some_and(|entry| entry.revision == session.revision() && entry.catalog == catalog) {
        if let Some(entry) = cache.entries.get_mut(&node.id) {
            entry.last_used = clock;
        }
        return cache.get(&node.id);
    }
    let admission::Admitted { descriptor, visible, enabled, roles, listed, bytes } = admission::admit(session, node, Some(cache))?;
    let (descriptor, borrowed_identity, bytes) = match descriptor {
        Cow::Borrowed(descriptor) => {
            // Safe pointer identity only: no raw-pointer dereference. Distinct borrowed variants
            // of one dynamic kind remain distinct; the registry outlives this view cache.
            let identity = descriptor as *const nodecraft_nodes::NodeDescriptor as usize;
            if let Some((shared, bytes, _)) = cache.shared.get(&identity) {
                (Arc::clone(shared), Some(identity), *bytes)
            } else {
                (Arc::new(descriptor.clone()), Some(identity), bytes)
            }
        }
        Cow::Owned(descriptor) => (Arc::new(descriptor), None, bytes),
    };
    cache.insert(
        node.id,
        CachedSchema {
            descriptor,
            visible,
            enabled,
            roles,
            listed,
            catalog,
            descriptor_bytes: bytes,
            borrowed_identity,
            revision: session.revision(),
            last_used: clock,
        },
    );
    while cache.bytes > MAX_SCHEMA_CACHE_BYTES {
        let oldest = cache.entries.iter().filter(|(id, _)| **id != node.id).min_by_key(|(_, entry)| entry.last_used).map(|(id, _)| *id);
        if let Some(id) = oldest {
            cache.remove(&id);
        } else {
            cache.remove(&node.id);
            return None;
        }
    }
    cache.get(&node.id)
}

struct Drag {
    id: NodeId,
    gesture: String,
    pos: nodecraft_graph::Position,
}

/// A Shift-drag box select in progress: its world-space start and the selection it adds to.
/// Escape cancels it; a cancelled box keeps suppressing primary-drag panning until release.
struct Marquee {
    start: Pos2,
    base: Selection,
    cancelled: bool,
}

/// A noodle being dragged from an output port, or picked up by its input end to reroute it.
struct Wiring {
    from: Endpoint,
    port_type: PortType,
    /// The existing edge being rerouted; dropping it on empty canvas disconnects it.
    moving: Option<EdgeId>,
}

/// A copied node's card rect, from its kind's resolved instance (it isn't on the canvas yet).
fn payload_rect(session: &Session, node: &nodecraft_graph::Node, t: &theme::Tokens) -> Rect {
    card::world_rect(node, admission::admit_uncached(session, node).as_ref().map(|a| a.descriptor.as_ref()), t)
}

/// Screen points within which a click selects a noodle.
const NOODLE_HIT: f32 = 6.0;

/// How many floating areas quick-add will inspect for the pointer before refusing the key. The
/// bound keeps the ownership scan O(areas); tests assert against this value so the cap and the
/// fixture can't drift apart (#267).
pub(crate) const MAX_QUICK_ADD_AREAS: usize = 256;

fn curves(canvas: &Canvas, painted: &BTreeMap<NodeId, Rect>, kinds: &nodecraft_nodes::Registry, t: &Tokens) -> Vec<(EdgeId, [Pos2; 4], Modality)> {
    canvas
        .edges()
        .values()
        .filter_map(|edge| {
            let from = canvas.nodes().get(&edge.from.node)?;
            let to = canvas.nodes().get(&edge.to.node)?;
            let port = from.outputs.iter().find(|p| p.name == edge.from.port)?;
            let a = noodle::port_anchor(from, card_geometry(painted, kinds, from, t), Side::Output, &edge.from.port)?;
            let b = noodle::port_anchor(to, card_geometry(painted, kinds, to, t), Side::Input, &edge.to.port)?;
            Some((edge.id, noodle::bezier(a, b), port.port_type.modality))
        })
        .collect()
}

/// Selection queries offered by a card's context menu.
enum SelectQuery {
    Upstream,
    Downstream,
    Kind(String),
}

/// A kind's `quick_add` binding: one ASCII letter or digit, optionally after `Shift+`.
/// Anything else (Escape, arrows, other modifiers, non-ASCII) is not bound.
fn quick_add_key(spec: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let (modifiers, key) = match spec.strip_prefix("Shift+") {
        Some(key) => (egui::Modifiers::SHIFT, key),
        None => (egui::Modifiers::NONE, spec),
    };
    let mut chars = key.chars();
    let single = chars.next().filter(|c| c.is_ascii_alphanumeric() && chars.next().is_none())?;
    egui::Key::from_name(&single.to_ascii_uppercase().to_string()).map(|key| (modifiers, key))
}

/// Noodle endpoints use painted rectangles when available. The static fallback preserves
/// crossing noodles and an active wiring source culled after pan/zoom; selection never uses it.
fn card_geometry(painted: &BTreeMap<NodeId, Rect>, kinds: &nodecraft_nodes::Registry, node: &nodecraft_graph::Node, t: &theme::Tokens) -> Rect {
    painted.get(&node.id).copied().unwrap_or_else(|| card::world_rect(node, kinds.get(&node.kind).ok().map(|kind| kind.descriptor()), t))
}

/// Cards a dragged noodle can drop on, with their world rects: only cards drawn this frame. A
/// card that wasn't drawn has no place on screen to drop on, and its static size could put a
/// phantom input inside the viewport.
fn drop_targets<'a>(canvas: &'a Canvas, painted: &'a BTreeMap<NodeId, Rect>) -> impl Iterator<Item = (&'a Node, Rect)> + 'a {
    painted.iter().filter_map(|(id, rect)| canvas.nodes().get(id).map(|node| (node, *rect)))
}

/// A card's world rect for box selection: as painted this frame, or for a card that wasn't drawn,
/// its resolved instance rect when the card could reach the box at all (the largest card at its
/// position). Cards that can't reach it skip the resolution.
fn box_geometry(painted: &BTreeMap<NodeId, Rect>, session: &Session, cache: &mut admission::GeometryCache, node: &Node, world: Rect, t: &Tokens) -> Rect {
    if let Some(rect) = painted.get(&node.id) {
        return *rect;
    }
    let largest = Rect::from_min_size(Pos2::new(node.pos.x, node.pos.y), Vec2::new(t.card_width, t.card_max_height));
    if !largest.intersects(world) {
        return largest;
    }
    cache.rect(session, node, t)
}
pub(crate) mod admission;

mod app_mode;
#[cfg(not(target_arch = "wasm32"))]
mod comfy_nodes;
mod comments;
mod deck_editor;
mod deck_presenter;
mod drop;
mod element_library_view;
mod elements;
mod export;
mod fields;
#[cfg(not(target_arch = "wasm32"))]
mod file_dialogs;
mod generate;
mod image_tools;
mod layer_editor;
mod layer_fonts;
mod layout;
mod library_view;
mod live;
mod mask_badges;
mod mask_editor;
mod mentions;
mod mesh_capture;
mod metrics;
mod models;
mod navigation;
#[cfg(not(target_arch = "wasm32"))]
mod option_fetch;
mod order;
#[cfg(not(target_arch = "wasm32"))]
mod ownership_panel;
#[cfg(not(target_arch = "wasm32"))]
mod paste_image;
#[cfg(not(target_arch = "wasm32"))]
pub mod perf_run;
mod plugin_cards;
mod queue_panel;
mod reader;
#[cfg(not(target_arch = "wasm32"))]
mod recovery;
mod runs;
mod search;
mod sharing;
mod sheet_view;
mod split;
mod stale;
mod stale_badges;
#[cfg(not(target_arch = "wasm32"))]
pub mod storage;
mod styles;
mod tags;
mod technique_menu;
mod timeline_editor;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod updates;
#[cfg(any(test, target_arch = "wasm32"))]
mod web_files;
#[cfg(any(test, target_arch = "wasm32"))]
mod web_journal;

pub struct NodeCraftApp {
    pub theme: ThemeKind,
    pub view: CanvasView,
    pub session: Session,
    pub canvas: CanvasId,
    pub selection: Selection,
    pub canvas_prefs: CanvasSettings,
    pub navigation: Navigation,
    pub search: crate::search::Search,
    pub tag_filter: crate::tags::TagFilter,
    tag_focus_ids: Vec<egui::Id>,
    tag_focused: bool,
    tag_session_identity: Option<Arc<()>>,
    tag_focus_pending: bool,
    tag_mailbox: Rc<RefCell<Option<serde_json::Value>>>,
    search_focused: bool,
    search_mailbox: Rc<RefCell<Option<serde_json::Value>>>,
    pub comments: crate::comments::Discussions,
    comments_mailbox: Rc<RefCell<Option<serde_json::Value>>>,
    navigation_mailbox: Rc<RefCell<Option<serde_json::Value>>>,
    #[cfg(not(target_arch = "wasm32"))]
    store: Option<nodecraft_engine::settings::SettingsStore>,
    #[cfg(not(target_arch = "wasm32"))]
    files: file_dialogs::FileDialogs,
    /// Project files in the browser (#118).
    #[cfg(any(test, target_arch = "wasm32"))]
    web_files: web_files::WebFiles,
    /// Unsaved work kept in the browser for recovery (#717); none when nothing can be kept.
    #[cfg(any(test, target_arch = "wasm32"))]
    web_recovery: Option<web_journal::WebRecovery>,
    pub viewport: Rect,
    pub notice: Option<String>,
    stale_snapshot: Option<stale::StaleSnapshot>,
    /// Which cards show a stale badge, as of a revision (#211).
    stale_badges: Option<stale_badges::StaleBadges>,
    mask_badges: Option<mask_badges::MaskBadges>,
    #[cfg(not(target_arch = "wasm32"))]
    drop_acquisition: drop::Acquisition,
    /// A copied image being read off the UI thread (#319).
    #[cfg(not(target_arch = "wasm32"))]
    image_paste: paste_image::ImagePaste,
    /// A browser drop being read (#319); native tests drive its import too.
    #[cfg(any(test, target_arch = "wasm32"))]
    web_drop: Option<drop::web::Pending>,
    editors: BTreeMap<NodeId, EditorState>,
    bulk: crate::bulk::Panel,
    drag: Option<Drag>,
    marquee: Option<Marquee>,
    /// Field zones' view state (#66).
    fields: fields::Fields,
    /// World rectangles painted this frame: authoritative for selection, noodle anchors,
    /// port drop targets and scoped host geometry, independent of schema-cache eviction.
    painted: BTreeMap<NodeId, Rect>,
    wiring: Option<Wiring>,
    /// The input order badge being dragged: its list input and edge.
    reordering: Option<(crate::order::Input, EdgeId)>,
    palette: Option<Palette>,
    /// The startup question about an earlier session's unsaved work (#408).
    #[cfg(not(target_arch = "wasm32"))]
    recovery: Option<recovery::RecoveryOffer>,
    /// Live recompute of action nodes (#88).
    live: live::Live,
    /// Recovered work's kept journal, removed once that work is saved (#408).
    #[cfg(not(target_arch = "wasm32"))]
    recovered: Option<recovery::RecoveredWork>,
    /// This session's recovery journal, beside which the open project file is recorded.
    #[cfg(not(target_arch = "wasm32"))]
    recovery_journal: Option<std::path::PathBuf>,
    /// The project file last recorded there (`None` until the first record).
    #[cfg(not(target_arch = "wasm32"))]
    recorded_location: Option<Option<std::path::PathBuf>>,
    /// The window title last sent, so it is only sent when it changes.
    title: String,
    /// Image previews on cards (#386).
    previews: crate::preview::Previews,
    /// Video posters and players on cards (#105).
    videos: crate::video::Videos,
    /// Waveforms and the one sound playing, on cards (#106).
    audios: crate::audio::Audios,
    /// 3D models on cards (#111): decoded models, each card's camera, and whether a renderer is
    /// installed to draw them.
    meshes: crate::mesh_view::Meshes,
    mesh_views: std::collections::HashMap<NodeId, crate::mesh_view::MeshView>,
    mesh_gpu: bool,
    /// eframe's renderer, to free 3D previews no card draws any more.
    mesh_render: Option<eframe::egui_wgpu::RenderState>,
    /// When the last timed frame started, for the frame interval (#151).
    last_frame: Option<u64>,
    /// Whether every frame asks for the next at once, to time presented frames (#526).
    frame_pump: bool,
    /// Whether the last frame asked for the next at once, so its interval was presented.
    pumped: bool,
    /// When the last timed first pass started: a discarded pass isn't a presented frame.
    last_presented: Option<u64>,
    frame_workload: Option<nodecraft_engine::perf::Workload>,
    /// The rows × columns last chosen for "Split into grid".
    split_size: (u32, u32),
    /// The split being picked: seen in this frame's open menu, shown on the card next frame.
    split_seen: Option<(NodeId, u32, u32)>,
    split_shown: Option<(NodeId, u32, u32)>,
    /// The "Turn into" menu after a new noodle was dropped on empty canvas.
    turn_into: Option<TurnInto>,
    model_choice: Option<models::ModelChoice>,
    /// The price confirmation a card's Generate opened (#83).
    run_confirm: Option<generate::RunConfirm>,
    /// The node whose image the toolbar's Expand shows.
    viewer: Option<NodeId>,
    /// The PDF reader, when open (#590).
    reader: Option<reader::Reader>,
    /// The mask editor (#102), while open.
    mask_editor: Option<mask_editor::MaskEditor>,
    layer_editor: Option<layer_editor::LayerEditor>,
    presenter: Option<deck_presenter::Presenter>,
    deck_editor: Option<deck_editor::DeckEditor>,
    timeline_editor: Option<timeline_editor::TimelineEditor>,
    /// App mode's form (#95), while it is open.
    app_mode: Option<app_mode::AppMode>,
    /// A result's save dialog (#557), while open.
    exports: export::Exports,
    /// Includes the closing pass, so release/shortcut events cannot leak to canvas owners.
    model_frame_owned: bool,
    /// egui drops focus on a bare Escape before the frame runs, so remember last frame's.
    text_focused: bool,
    /// Exact previous text owner survives Escape and disabled-toolbar interaction.
    last_text_focus: Option<egui::Id>,
    schemas: SchemaCache,
    /// The cards prompts can `@mention`, and blocked mentions (#505).
    mention_cache: mentions::Cache,
    marquee_geometry: admission::GeometryCache,
    cache_clock: u64,
    next_gesture: u64,
    centred: bool,
    /// Media cards draw their prompt overlays (#321). `H` hides them for a clean board. View
    /// state only: it is never a document edit, a changeset or a saved setting, so a hidden
    /// overlay leaves `session.revision()` and undo/redo exactly as they were.
    prompt_overlays: bool,
    /// The API keys panel, and the accounts it lists (from settings).
    pub keys: crate::keys::KeysPanel,
    /// The plugin packages panel (#172).
    pub plugin_panel: crate::plugins::PluginsPanel,
    pub key_accounts: nodecraft_engine::settings::KeySettings,
    /// Answers from the keychain thread (the app's `keys.store` service).
    #[cfg(not(target_arch = "wasm32"))]
    key_results: Option<nodecraft_engine::secrets::KeyResults>,
    /// The runs `run.start` hands over, and the providers they use (#409).
    runs: Option<runs::Runs>,
    /// Fetches the option lists `options.refresh` asks for (#529).
    #[cfg(not(target_arch = "wasm32"))]
    option_fetch: Option<option_fetch::OptionFetcher>,
    /// Reads the ComfyUI server's nodes `comfy.nodes.refresh` asks for (#9).
    #[cfg(not(target_arch = "wasm32"))]
    comfy_nodes: Option<comfy_nodes::ComfyNodesFetcher>,
    /// Checks for a newer release (#123).
    #[cfg(not(target_arch = "wasm32"))]
    updates: Option<updates::UpdateChecker>,
    /// Shares the project live through a relay, or joins one (#698), in the browser too (#699).
    sharing: Option<sharing::Sharing>,
    /// The plugin worker, and the change of what may run the session last took from it (#173).
    #[cfg(not(target_arch = "wasm32"))]
    plugins: Option<(Arc<nodecraft_engine::plugin_cmds::PluginWorker>, Option<u64>)>,
    /// The queue panel is open (#82).
    queue_open: bool,
    /// The quarantined-charge review, when open (#362).
    #[cfg(not(target_arch = "wasm32"))]
    ownership: Option<ownership_panel::OwnershipReview>,
    /// The sheets panel and the open sheet's table (#163).
    sheet: sheet_view::SheetView,
    /// The Techniques panel (#96).
    library: library_view::LibraryView,
    element_library: element_library_view::ElementLibraryView,
}
impl NodeCraftApp {
    /// Cancel one owner at a time: noodle, input order, marquee, then selection.
    fn cancel_canvas_gesture(&mut self) {
        if self.fields.drag.take().is_some() {
            return;
        }
        if self.wiring.take().is_some() {
            return;
        }
        if self.reordering.take().is_some() {
            return;
        }
        match &mut self.marquee {
            Some(marquee) if !marquee.cancelled => {
                marquee.cancelled = true;
                self.marquee_geometry.clear();
                self.selection = marquee.base.clone();
            }
            _ => {
                self.selection.clear();
                self.fields.selected = None;
            }
        }
    }

    pub fn new(ctx: &egui::Context) -> Result<Self, Error> {
        #[cfg(not(target_arch = "wasm32"))]
        let mut app = Self::headless()?;
        #[cfg(target_arch = "wasm32")]
        let (mut app, accounts) = Self::browser()?;
        app.apply_settings(ctx, &NodeCraftSettings::default());
        // No settings file remembers the accounts in the browser: list the ones with a stored key.
        #[cfg(target_arch = "wasm32")]
        for account in &accounts {
            app.key_accounts.remember(account);
        }
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        Ok(app)
    }
    /// The app with settings from `store`, which it saves while running and flushes on exit.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_settings(ctx: &egui::Context, store: nodecraft_engine::settings::SettingsStore) -> Result<Self, Error> {
        Self::with_settings_and_storage(ctx, store, None)
    }

    /// [`Self::with_settings`], with project files when `storage` is given (#406): save and open
    /// work, edits are journalled for recovery, and `storage.open` is opened at startup.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_settings_and_storage(
        ctx: &egui::Context,
        mut store: nodecraft_engine::settings::SettingsStore,
        storage: Option<storage::StorageSetup>,
    ) -> Result<Self, Error> {
        // Made once per install and saved at once, before any provider is built (#409).
        if let Err(error) = nodecraft_engine::settings::ensure_install_salt(&mut store) {
            log::warn!("{}", error.message);
        }
        let settings = store.get().clone();
        // Keychain calls run on their own thread, so an unlock prompt never freezes the window.
        // If the thread can't start, the app runs without the keychain (the panel says so).
        let mut services = nodecraft_engine::commands::Services::default();
        let results = match nodecraft_engine::secrets::KeyWorker::spawn(std::sync::Arc::new(nodecraft_engine::secrets::KeyringStore)) {
            Ok((worker, results)) => {
                nodecraft_engine::secrets::register_key_worker(&mut services, worker)?;
                Some(results)
            }
            Err(e) => {
                log::warn!("API keys unavailable: {}", e.message);
                None
            }
        };
        // Without them the app still opens; Generate says why it can't run.
        let mut runs = match runs::Runs::new(&mut services, std::sync::Arc::new(nodecraft_engine::secrets::KeyringStore)) {
            Ok(runs) => Some(runs),
            Err(e) => {
                log::warn!("runs unavailable: {}", e.message);
                None
            }
        };
        let option_fetch = match option_fetch::OptionFetcher::new(&mut services) {
            Ok(fetcher) => Some(fetcher),
            Err(e) => {
                log::warn!("option lists can't be fetched: {}", e.message);
                None
            }
        };
        let comfy_nodes = match comfy_nodes::ComfyNodesFetcher::new(&mut services) {
            Ok(fetcher) => Some(fetcher),
            Err(e) => {
                log::warn!("ComfyUI workflows can't be imported: {}", e.message);
                None
            }
        };
        // Updates are staged in, and their outcome recorded in, the settings folder (#540).
        let mut updates = match updates::UpdateChecker::new(&mut services, store.dir().map(|d| d.join("updates"))) {
            Ok(checker) => Some(checker),
            Err(e) => {
                log::warn!("updates can't be checked: {}", e.message);
                None
            }
        };
        let sharing = Self::register_sharing(&mut services, Box::new(sharing::open_window));
        // Preserve the existing plugin/trust location. All package I/O, including opening
        // its index, runs on the worker; requests expose retained results through commands.
        let mut plugin_problem = None;
        let mut plugins = None;
        if let Some(dir) = store.dir().map(|d| d.join("plugins")) {
            match Self::register_plugins(&mut services, dir) {
                Ok(worker) => plugins = Some((worker, None)),
                Err(error) => {
                    log::warn!("plugin packages unavailable: {}", error.message);
                    plugin_problem = Some(format!("Plugin packages are unavailable: {}", error.message));
                }
            }
        }
        // Storage that can't start (another window holds the journal, or the project won't
        // open) still opens the window, without project files, and says why.
        let mut problem = None;
        let journal = storage.as_ref().map(|setup| setup.recovery.clone());
        let runs_journal = journal.as_ref().and_then(|recovery| recovery.parent()).map(|dir| dir.join("runs.json"));
        let stored = storage.and_then(|setup| match Self::storage_session(setup) {
            Ok(stored) => Some(stored),
            Err(error) => {
                log::error!("project storage unavailable: {}", error.message);
                problem = Some(format!("Project files are unavailable: {}", error.message));
                None
            }
        });
        let mut kept = None;
        let owns_storage = stored.is_some();
        let mut app = match stored {
            Some((session, canvas, unrecovered, open_error)) => {
                kept = unrecovered;
                if let Some(error) = open_error {
                    problem = Some(format!("That project couldn't be opened: {}", error.message));
                }
                Self::from_session(session.with_services(services), canvas)?
            }
            None => Self::with_services(services)?,
        };
        app.notice = problem.or(plugin_problem);
        app.plugins = plugins;
        // Unsaved work from a session that didn't close cleanly: ask before anything else (#408).
        if let Some(journal) = journal {
            app.attach_recovery(journal, kept);
        }
        app.key_results = results;
        if let Some(runs) = &mut runs {
            // Only a window that owns project storage (and so its folder's lock) keeps runs.
            if owns_storage && let Some(path) = runs_journal {
                runs.open_journal(path);
            }
            runs.install(&mut app.session);
        }
        app.runs = runs;
        app.option_fetch = option_fetch;
        app.comfy_nodes = comfy_nodes;
        // The technique library every project shares (#96), beside the plugins; in memory when
        // settings aren't kept on disk.
        let library: Box<dyn nodecraft_engine::library::LibraryStore> = match store.dir() {
            Some(dir) => Box::new(nodecraft_engine::library::FileLibrary::new(dir.join("library"))),
            None => Box::new(nodecraft_engine::library::MemoryLibrary::default()),
        };
        if let Err(error) = nodecraft_engine::library::install(&mut app.session, library) {
            log::warn!("technique library unavailable: {}", error.message);
        }
        // The Elements library (#552), beside it.
        let elements: std::rc::Rc<dyn nodecraft_engine::element_library::ElementStore> = match store.dir() {
            Some(dir) => std::rc::Rc::new(nodecraft_engine::element_library::FileElements::new(dir.join("elements"))),
            None => std::rc::Rc::new(nodecraft_engine::element_library::MemoryElements::default()),
        };
        nodecraft_engine::element_library::install(&mut app.session, elements);
        // The person's agent skills (#168), beside the library.
        let skills: std::rc::Rc<dyn nodecraft_engine::skills::SkillStore> = match store.dir() {
            Some(dir) => std::rc::Rc::new(nodecraft_engine::skills::FileSkills::new(dir.join("skills"))),
            None => std::rc::Rc::new(nodecraft_engine::skills::MemorySkills::default()),
        };
        if let Err(error) = nodecraft_engine::skills::install(&mut app.session, skills) {
            log::warn!("your skills are unavailable: {}", error.message);
        }
        if let Some(updates) = &mut updates
            && let Some(told) = updates.after_update.take()
        {
            app.notice = Some(app.notice.map_or(told.clone(), |notice| format!("{told} {notice}")));
        }
        app.updates = updates;
        app.sharing = sharing;
        app.check_updates_at_startup(&settings.updates);
        app.apply_settings(ctx, &NodeCraftSettings::default());
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        app.apply_settings(ctx, &settings);
        app.store = Some(store);
        Ok(app)
    }

    /// Takes the values the shell uses from `settings` and restyles `ctx` for the theme.
    pub fn apply_settings(&mut self, ctx: &egui::Context, settings: &NodeCraftSettings) {
        self.theme = match settings.appearance.theme {
            Theme::Dark => ThemeKind::Dark,
            Theme::Light => ThemeKind::Light,
        };
        self.canvas_prefs = settings.canvas.clone();
        self.key_accounts = settings.keys.clone();
        self.set_media_tools(settings.media.tools());
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.files.projects = nodecraft_engine::settings::ProjectSettings { recent: settings.projects.recent() };
        }
        if let Some(sharing) = self.sharing.as_mut() {
            sharing.set_relay(settings.share.relay.clone());
        }
        theme::apply(ctx, self.theme);
    }

    /// Configure playback and rendering together from trusted host settings or startup discovery.
    /// The native host resolves paths before the frame loop; this does not inspect the filesystem.
    pub fn set_media_tools(&mut self, tools: Option<(&std::path::Path, &std::path::Path)>) {
        self.videos.configure(tools);
        self.audios.configure(tools);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(runs) = self.runs.as_mut() {
            runs.set_video_tools(tools);
        }
    }

    /// The app with no host services but a technique library kept in memory.
    pub fn headless() -> Result<Self, Error> {
        Self::headless_with_library(Box::new(nodecraft_engine::library::MemoryLibrary::default()))
    }

    /// [`Self::headless`] keeping its technique library (#96) in `store`, and the person's skills
    /// (#168) in memory.
    pub fn headless_with_library(store: Box<dyn nodecraft_engine::library::LibraryStore>) -> Result<Self, Error> {
        Self::in_memory(nodecraft_engine::commands::Services::default(), store)
    }

    /// The app on `services`, with its technique library in `store` and skills in memory.
    fn in_memory(services: nodecraft_engine::commands::Services, store: Box<dyn nodecraft_engine::library::LibraryStore>) -> Result<Self, Error> {
        let mut app = Self::with_services(services)?;
        nodecraft_engine::library::install(&mut app.session, store)?;
        nodecraft_engine::element_library::install(&mut app.session, std::rc::Rc::new(nodecraft_engine::element_library::MemoryElements::default()));
        nodecraft_engine::skills::install(&mut app.session, std::rc::Rc::new(nodecraft_engine::skills::MemorySkills::default()))?;
        Ok(app)
    }

    /// The browser build (#118): provider keys kept in this origin's browser storage, answered at
    /// once (no keychain prompt can block). Also returns the accounts that already have a key.
    #[cfg(target_arch = "wasm32")]
    fn browser() -> Result<(Self, Vec<nodecraft_engine::secrets::Account>), Error> {
        use nodecraft_engine::secrets::{OriginStore, SecretStore, register_key_service};
        let store = std::sync::Arc::new(OriginStore::new(crate::web_storage::LocalStorage));
        let mut services = nodecraft_engine::commands::Services::default();
        register_key_service(&mut services, std::sync::Arc::clone(&store) as std::sync::Arc<dyn SecretStore>)?;
        let accounts = store.accounts().unwrap_or_else(|e| {
            log::warn!("API keys unavailable: {e}");
            Vec::new()
        });
        // Runs call providers straight from the page (#118), with the keys kept here. Without
        // them the app still opens; Generate says why it can't run.
        let runs = runs::Runs::new(&mut services, std::sync::Arc::clone(&store) as std::sync::Arc<dyn SecretStore>)
            .inspect_err(|e| log::warn!("runs unavailable: {}", e.message))
            .ok();
        let recovery = web_journal::WebRecovery::new(Box::<web_journal::IndexedDb>::default(), web_journal::fresh_slot());
        // Live sharing through a relay over the page's WebSocket (#699).
        let sharing = Self::register_sharing(&mut services, Box::new(sharing::open_window));
        let mut app = Self::with_web_files(services, web_files::WebFiles::browser(), Some(recovery))?;
        app.sharing = sharing;
        if let Some(runs) = &runs {
            runs.install(&mut app.session);
        }
        app.runs = runs;
        // Recovery is a safety net, not a save: leaving with unsaved work still asks first.
        web_files::guard_unload(Rc::clone(&app.web_files.unsaved));
        Ok((app, accounts))
    }

    /// The in-canvas agent's settings (#114), or the defaults without a settings file.
    pub fn agent_settings(&self) -> nodecraft_engine::settings::AgentSettings {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &self.store {
            return store.get().agent.clone();
        }
        nodecraft_engine::settings::AgentSettings::default()
    }

    /// Changes the agent's settings; they are saved with the others. Without a settings file the
    /// change is dropped.
    pub fn update_agent_settings(&mut self, change: impl FnOnce(&mut nodecraft_engine::settings::AgentSettings)) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store {
            store.update(|settings| change(&mut settings.agent));
        }
        #[cfg(target_arch = "wasm32")]
        let _ = change;
    }

    /// The `plugins` service over the package store in `dir` (`store/`, `trust.json`). No
    /// package may take an ID the app registers; no host implementation is bindable yet.
    #[cfg(not(target_arch = "wasm32"))]
    fn register_plugins(
        services: &mut nodecraft_engine::commands::Services,
        dir: std::path::PathBuf,
    ) -> Result<Arc<nodecraft_engine::plugin_cmds::PluginWorker>, Error> {
        use nodecraft_engine::plugin_cmds::{PluginHost, PluginWorker, register_plugin_worker};
        let mut taken: std::collections::BTreeSet<String> = nodecraft_engine::commands::Registry::builtins()?.specs().map(|s| s.id.to_owned()).collect();
        taken.extend(nodecraft_engine::generation_catalog::default_registry()?.0.descriptors().map(|d| d.id.clone()));
        let host = PluginHost { trust: Some(dir.join("trust.json")), taken, bindings: Default::default() };
        let worker = PluginWorker::spawn(move || nodecraft_plugin_store::Store::open(nodecraft_plugin_store::FolderBackend::new(dir.join("store"))?), host)?;
        register_plugin_worker(services, Arc::clone(&worker))?;
        Ok(worker)
    }

    /// Hands the session what the plugin store may run whenever that changes (#173); the first
    /// frame asks the worker to read the store.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_plugins(&mut self, ctx: &egui::Context) {
        let Some((worker, seen)) = &mut self.plugins else { return };
        // Until the first refresh is queued, ask again each frame.
        if seen.is_none() && !worker.refresh() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        let changes = worker.changes();
        if *seen == Some(changes) {
            return;
        }
        let (changes, loaded, plugins) = worker.published();
        *seen = Some(changes);
        let before = Arc::clone(self.session.plugins());
        let published = self.session.set_plugins(plugins);
        if let Err(error) = &published {
            log::warn!("plugin actions unavailable: {}", error.message);
        }
        if !Arc::ptr_eq(&before, self.session.plugins()) {
            self.plugins_changed();
        }
        // Even when nothing changed: an empty store, once read, tells restored jobs their plugin
        // is gone. Plugins read in part, or that couldn't be published, aren't known.
        if let Some(Err(error)) = self.runs.as_mut().map(|runs| runs.set_plugins(&mut self.session, loaded && published.is_ok())) {
            log::warn!("plugin providers unavailable: {}", error.message);
        }
        ctx.request_repaint();
    }

    /// The headless app with these host services on its session (the keychain, for one).
    pub fn with_services(services: nodecraft_engine::commands::Services) -> Result<Self, Error> {
        let kinds = nodecraft_engine::generation_catalog::default_registry()?.0;
        let (document, canvas) = Self::starter_document(&kinds)?;
        let session = Session::new(document)?.with_services(services);
        Self::from_session(session, canvas)
    }

    /// ⌘S, ⇧⌘S and ⌘O in the browser (#118).
    #[cfg(target_arch = "wasm32")]
    fn file_shortcut(&mut self, ctx: &egui::Context, save: bool, save_as: bool) -> Option<String> {
        let id = if save_as {
            "file.save_as"
        } else if save {
            "file.save"
        } else {
            "file.open"
        };
        self.notice = None;
        self.web_file(id, ctx);
        self.notice.take()
    }

    /// After a project replaced the document: its first canvas, nothing selected or edited.
    fn after_file_open(&mut self) {
        if let Some(canvas) = sheet_view::home_canvas(self.session.document()) {
            self.canvas = canvas;
        }
        self.selection.clear();
        self.editors.clear();
        self.centred = false;
    }

    /// The project a new window starts with: one canvas holding a sample Text node.
    fn starter_document(kinds: &nodecraft_nodes::Registry) -> Result<(Project, CanvasId), Error> {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Untitled")?;
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document)?;
        let mut values = BTreeMap::new();
        values.insert("text".into(), Value::Text("A quiet forest at dawn, soft light through the trees.".into()));
        let mut node = kinds.get("source.text")?.descriptor().instantiate(NodeId::from_bytes([3; 16]), &values)?;
        node.pos = nodecraft_graph::Position { x: -160.0, y: -110.0 };
        Change::AddNode { canvas, node }.apply(&mut document)?;
        Ok((document, canvas))
    }
    pub fn from_session(mut session: Session, canvas: CanvasId) -> Result<Self, Error> {
        if !session.document().canvases().contains_key(&canvas) {
            return Err(Error::new(ErrorKind::NotFound, "canvas does not exist"));
        }
        metrics::install_clock(&mut session);
        let navigation_mailbox = Rc::new(RefCell::new(None));
        let mailbox = Rc::clone(&navigation_mailbox);
        session.register_host_service(nodecraft_engine::navigation_cmds::SERVICE, move |request| {
            let mut slot = mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "navigation host is busy"))?;
            if slot.is_some() {
                return Err(Error::new(ErrorKind::Conflict, "navigation request is already pending"));
            }
            *slot = Some(request.clone());
            Ok(json!({"accepted":true}))
        })?;
        let search_mailbox = Rc::new(RefCell::new(None));
        let mailbox = Rc::clone(&search_mailbox);
        session.register_host_service(nodecraft_engine::search_cmds::SERVICE, move |request| {
            let mut slot = mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "search host is busy"))?;
            if slot.is_some() {
                return Err(Error::new(ErrorKind::Conflict, "search request is already pending"));
            }
            *slot = Some(request.clone());
            Ok(json!({"accepted":true}))
        })?;
        let comments_mailbox = Rc::new(RefCell::new(None));
        let mailbox = Rc::clone(&comments_mailbox);
        session.register_host_service(nodecraft_engine::comment_cmds::SERVICE, move |request| {
            let mut slot = mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "discussions host is busy"))?;
            if slot.is_some() {
                return Err(Error::new(ErrorKind::Conflict, "discussion request is already pending"));
            }
            *slot = Some(request.clone());
            Ok(json!({"accepted":true}))
        })?;
        let tag_mailbox = Rc::new(RefCell::new(None));
        let mailbox = Rc::clone(&tag_mailbox);
        session.register_host_service(nodecraft_engine::tag_filter_cmds::SERVICE, move |request| {
            let mut slot = mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "tag host is busy"))?;
            if slot.is_some() {
                return Err(Error::new(ErrorKind::Conflict, "tag request is already pending"));
            }
            *slot = Some(request.clone());
            Ok(serde_json::json!({"accepted":true}))
        })?;
        Ok(Self {
            theme: ThemeKind::default(),
            view: CanvasView::default(),
            session,
            canvas,
            selection: Selection::default(),
            canvas_prefs: CanvasSettings::default(),
            navigation: Navigation::default(),
            search: crate::search::Search::default(),
            tag_filter: crate::tags::TagFilter::default(),
            tag_focus_ids: Vec::new(),
            tag_focused: false,
            tag_session_identity: None,
            tag_focus_pending: false,
            tag_mailbox,
            search_focused: false,
            search_mailbox,
            comments: crate::comments::Discussions::default(),
            comments_mailbox,
            navigation_mailbox,
            #[cfg(not(target_arch = "wasm32"))]
            store: None,
            #[cfg(not(target_arch = "wasm32"))]
            files: Default::default(),
            #[cfg(any(test, target_arch = "wasm32"))]
            web_files: web_files::WebFiles::detached(),
            #[cfg(any(test, target_arch = "wasm32"))]
            web_recovery: None,
            viewport: Rect::NOTHING,
            notice: None,
            stale_snapshot: None,
            stale_badges: None,
            mask_badges: None,
            #[cfg(not(target_arch = "wasm32"))]
            drop_acquisition: drop::Acquisition::new(),
            #[cfg(not(target_arch = "wasm32"))]
            image_paste: paste_image::ImagePaste::new(),
            #[cfg(any(test, target_arch = "wasm32"))]
            web_drop: None,
            editors: BTreeMap::new(),
            bulk: Default::default(),
            drag: None,
            marquee: None,
            fields: fields::Fields::default(),
            painted: BTreeMap::new(),
            wiring: None,
            reordering: None,
            palette: None,
            #[cfg(not(target_arch = "wasm32"))]
            recovery: None,
            live: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            recovered: None,
            #[cfg(not(target_arch = "wasm32"))]
            recovery_journal: None,
            #[cfg(not(target_arch = "wasm32"))]
            recorded_location: None,
            title: String::new(),
            previews: crate::preview::Previews::default(),
            videos: crate::video::Videos::default(),
            audios: crate::audio::Audios::default(),
            meshes: crate::mesh_view::Meshes::default(),
            mesh_views: std::collections::HashMap::new(),
            mesh_gpu: false,
            mesh_render: None,
            last_frame: None,
            frame_pump: false,
            pumped: false,
            last_presented: None,
            frame_workload: None,
            split_size: (2, 2),
            split_seen: None,
            split_shown: None,
            turn_into: None,
            model_choice: None,
            run_confirm: None,
            viewer: None,
            reader: None,
            mask_editor: None,
            layer_editor: None,
            presenter: None,
            deck_editor: None,
            timeline_editor: None,
            app_mode: None,
            exports: Default::default(),
            model_frame_owned: false,
            text_focused: false,
            last_text_focus: None,
            schemas: SchemaCache::default(),
            mention_cache: mentions::Cache::default(),
            marquee_geometry: admission::GeometryCache::default(),
            cache_clock: 0,
            next_gesture: 0,
            centred: false,
            prompt_overlays: true,
            keys: crate::keys::KeysPanel::default(),
            plugin_panel: crate::plugins::PluginsPanel::default(),
            key_accounts: nodecraft_engine::settings::KeySettings::default(),
            #[cfg(not(target_arch = "wasm32"))]
            key_results: None,
            runs: None,
            #[cfg(not(target_arch = "wasm32"))]
            option_fetch: None,
            #[cfg(not(target_arch = "wasm32"))]
            comfy_nodes: None,
            #[cfg(not(target_arch = "wasm32"))]
            updates: None,
            sharing: None,
            #[cfg(not(target_arch = "wasm32"))]
            plugins: None,
            queue_open: false,
            #[cfg(not(target_arch = "wasm32"))]
            ownership: None,
            sheet: sheet_view::SheetView::default(),
            library: library_view::LibraryView::default(),
            element_library: element_library_view::ElementLibraryView::default(),
        })
    }
    /// Replace a document session together with scoped view services and all view caches.
    /// Assigning the public session field directly is not a supported live-host replacement path.
    pub fn replace_session(&mut self, session: Session, canvas: CanvasId) -> Result<(), Error> {
        let mut next = Self::from_session(session, canvas)?;
        next.theme = self.theme;
        next.canvas_prefs = self.canvas_prefs.clone();
        // Retain bounded dirty/orphan discussion drafts with their original Session token.
        // New host services and view caches belong to the replacement Session.
        let mut discussions = std::mem::take(&mut self.comments);
        discussions.reset_session_view();
        next.comments = discussions;
        #[cfg(not(target_arch = "wasm32"))]
        {
            next.store = self.store.take();
            // An OS picker is not cancelled by dropping its future. Keep its occupied
            // slot and original Session stamp until completion, plus bounded recents.
            next.files = std::mem::take(&mut self.files);
        }
        *self = next;
        Ok(())
    }
    pub fn home(&mut self) -> Pos2 {
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        self.navigation.geometry.rects.values().copied().reduce(|a, b| a.union(b)).map_or(Pos2::ZERO, |r| r.center())
    }
    /// One frame of the canvas, timed for performance capture when it's running (#151).
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let started = self.frame_started(ui.ctx().current_pass_index() == 0);
        self.show_frame(ui);
        self.frame_finished(started);
        self.pumped = self.frame_pump && self.session.perf().enabled();
        if self.pumped {
            ui.ctx().request_repaint();
        }
    }

    fn show_frame(&mut self, ui: &mut egui::Ui) {
        self.keep_canvas();
        // Dismissing or choosing from a palette must not replay another key from the same
        // input frame as a canvas shortcut. Keep ownership even after palette.show closes it.
        let palette_owned_input = self.palette.is_some();
        // The "Turn into" menu takes letters for kinds exactly as a text field does, and its entry
        // keys are those kinds' quick-add letters. Real backends deliver `Event::Key` and
        // `Event::Text` in one raw-input frame, so one keypress arrives in this same frame: a pick
        // closes the menu and clears `self.turn_into` before `quick_add` runs at the end of the
        // frame, and the inline `turn_into.is_some()` gate would be too late. Keep the ownership
        // frozen even after the menu closes, so one keypress creates one node (#267).
        let turn_into_owned_input = self.turn_into.is_some();
        #[cfg(not(target_arch = "wasm32"))]
        let recovery_owns_input = self.recovery.is_some();
        #[cfg(target_arch = "wasm32")]
        let recovery_owns_input = self.web_recovery_owns_input();
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.poll_save(ui.ctx());
            let t = *self.theme.tokens();
            self.recovery_dialog(ui.ctx(), &t);
            self.track_recovery();
            // Not while the startup question is open: its answer may replace the document.
            if self.recovery.is_none() {
                self.live_recompute(ui.ctx());
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.web_recovery_frame(ui.ctx());
            // Not while the load's recovery question is pending: its answer may replace the
            // document.
            if !self.web_recovery_owns_input() {
                self.live_recompute(ui.ctx());
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let file_owns_input = self.file_dialog_frame(ui.ctx());
        #[cfg(target_arch = "wasm32")]
        let file_owns_input = self.web_files_frame(ui.ctx());
        // The grid picked in last frame's menu shows this frame; it goes when the menu closes.
        self.split_shown = self.split_seen.take();
        self.previews.poll(ui.ctx());
        self.poll_export(ui.ctx());
        self.videos.poll(ui.ctx());
        self.audios.poll(ui.ctx());
        self.meshes.poll();
        if let Some(render) = &self.mesh_render {
            crate::mesh_view::prune(render, ui.ctx().cumulative_frame_nr());
        }
        let surrender_focus_on = ui.ctx().options(|o| o.input_options.surrender_focus_on);
        self.cache_clock = self.cache_clock.saturating_add(1);
        let changed_session = self.marquee_geometry.prepare(&mut self.session, self.canvas, self.theme);
        if changed_session {
            self.schemas = SchemaCache::default();
        }
        if self.marquee.as_ref().is_none_or(|m| m.cancelled) {
            self.marquee_geometry.clear();
        }
        self.schemas.refresh_generation(&self.session);
        self.fields_sync();
        let t = self.theme.tokens();
        let viewport = ui.max_rect();
        self.viewport = viewport;
        #[cfg(not(target_arch = "wasm32"))]
        let file_menu_owns_input = self.file_menu(ui, t);
        #[cfg(target_arch = "wasm32")]
        let file_menu_owns_input = false;
        #[cfg(not(target_arch = "wasm32"))]
        if file_menu_owns_input {
            self.clear_file_input(ui.ctx());
        }
        if !self.centred {
            let home = self.home();
            self.view.reset(viewport, home);
            self.centred = true;
        }
        let mut model_blocks = self.model_overlay(ui);
        // Before the price confirmation, which a run from the form opens on top of it.
        model_blocks |= self.app_overlay(ui);
        model_blocks |= self.run_overlay(ui);
        model_blocks |= self.mask_overlay(ui);
        model_blocks |= self.layer_overlay(ui);
        model_blocks |= self.deck_editor_overlay(ui);
        model_blocks |= self.deck_overlay(ui);
        model_blocks |= self.timeline_overlay(ui);
        if self.on_sheet() {
            self.sheet_frame(ui, viewport, model_blocks || file_owns_input || file_menu_owns_input);
            ui.ctx().options_mut(|o| o.input_options.surrender_focus_on = surrender_focus_on);
            return;
        }
        let bulk_enabled = !model_blocks
            && !file_owns_input
            && !file_menu_owns_input
            && self.session.file_status().operation.is_none()
            && !self.drop_pointer_owned()
            && self.drag.is_none()
            && self.marquee.is_none()
            && self.wiring.is_none()
            && self.reordering.is_none()
            && self.comments.drag.is_none()
            && !self.comments.gesture_cancelled
            && !self.navigation.pointer_owned
            && self.navigation.drag.is_none()
            && self.palette.is_none()
            && self.turn_into.is_none()
            && !self.keys.open
            && (!canvas::text_input_focused(ui.ctx()) || self.bulk.owns_text_focus(ui.ctx()))
            && (!egui::Popup::is_any_open(ui.ctx()) || self.bulk.owns_popup(ui.ctx()))
            && ui.input(|i| i.raw.hovered_files.is_empty() && i.raw.dropped_files.is_empty());
        let (bulk_blocks, bulk_request) = self.bulk.show(ui, &self.session, self.canvas, self.selection.nodes(), bulk_enabled, t);
        if bulk_enabled && let Some(request) = bulk_request {
            let result = match request {
                crate::bulk::Intent::Parameters(params) => self.session.dispatch("node.params_set_bulk", &params).map(|_| ()),
                crate::bulk::Intent::OpenModel { canvas, exact_ids, revision } => {
                    let result = self.open_model_picker(canvas, exact_ids, revision);
                    if result.is_ok() {
                        model_blocks = true;
                    }
                    result
                }
            };
            match result {
                Ok(()) => self.notice = None,
                Err(error) => {
                    self.bulk.failed(error.message.clone());
                    self.notice = Some(error.message);
                }
            }
        }
        let comments_blocks = if model_blocks { true } else { self.comments_overlay(ui) };
        // A context menu is not permission to commit a dirty editor draft.
        if self.text_focused
            && !egui::Popup::is_any_open(ui.ctx())
            && let Some(owner) = self.last_text_focus
            && ui.ctx().memory(|m| m.focused()).is_none_or(|id| id == owner)
            && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary) || i.pointer.button_released(egui::PointerButton::Secondary))
            && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            && ui.ctx().layer_id_at(pointer) == Some(ui.layer_id())
            && self.painted.values().any(|r| self.view.rect_to_screen(viewport.min, *r).contains(pointer))
        {
            ui.ctx().memory_mut(|m| m.request_focus(owner));
            ui.ctx().options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        let tag_blocks = if model_blocks { true } else { self.tag_overlay(ui) };
        let drop_blocks = self.poll_drop(ui, t);
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_image_paste(ui.ctx());
        let search_blocks = if model_blocks { true } else { self.search_overlay(ui) };
        let selected_before = self.selection.clone();
        let mut minimap_press = None;
        // Save and open (#406) work from a text field too (no field uses them); not while the palette
        // or "Turn into" is open. Without a file yet, saving needs a location: say how to give one.
        if !model_blocks && self.palette.is_none() && self.turn_into.is_none() {
            let (save_as, save, open) =
                ui.input_mut(|i| (consume_shortcut(i, "Command+Shift+S"), consume_shortcut(i, "Command+S"), consume_shortcut(i, "Command+O")));
            if save || save_as || open {
                self.notice = self.file_shortcut(ui.ctx(), save, save_as);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.files.owns_input() {
            self.clear_file_input(ui.ctx());
        }
        // Canvas keys yield to text fields and to the open palette (whose query may lose focus)
        // or "Turn into" menu (whose letters pick kinds).
        if !model_blocks && !self.drop_pointer_owned() && !canvas::text_input_focused(ui.ctx()) && self.palette.is_none() && self.turn_into.is_none() {
            // Held minimap input owns mutation keys through release. Fresh presses still
            // use the post-undo/delete admission below, since those edits can change availability.
            let minimap_held = self.navigation.pointer_owned || self.navigation.drag.is_some();
            let command = if minimap_held {
                None
            } else {
                ui.input_mut(|input| {
                    self.session.registry().specs().filter(|spec| matches!(spec.id, "edit.undo" | "edit.redo")).find_map(|spec| {
                        spec.shortcut
                            .into_iter()
                            .chain(spec.alternate_shortcuts.iter().copied())
                            .any(|shortcut| consume_shortcut(input, shortcut))
                            .then_some(spec.id)
                    })
                })
            };
            if let Some(command) = command {
                if let Err(error) = self.session.dispatch(command, &json!({})) {
                    self.notice = Some(error.message);
                } else {
                    self.notice = None;
                    // Invalid drafts remain view state across unrelated undo/redo.
                }
            }
            // `H` hides prompt overlays on media cards (#321). It is a plan §2.1 canvas letter, so
            // it yields to exactly what the quick-add letters yield to — a focused text field, the
            // palette, the "Turn into" menu, an open popup, a panel that owns canvas input and a
            // gesture in flight — and it is never taken from any of them unconsumed.
            let overlays_offered = !self.canvas_letter_keys_blocked(ui, minimap_held, model_blocks, bulk_blocks);
            let (select_all, escape, delete, overlays) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                    i.key_pressed(egui::Key::Escape),
                    !minimap_held && (i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)),
                    overlays_offered && consume_canvas_letter(i, egui::Key::H),
                )
            });
            if overlays && let Err(error) = self.dispatch_navigation(ui.ctx(), "canvas.prompt_overlays_toggle", &json!({})) {
                self.notice = Some(error.message);
            }
            if delete && let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
                // Drop ids an undo or canvas switch removed, so they don't fail the delete.
                self.selection.retain_existing(canvas);
            }
            if delete && !self.text_focused {
                self.dispatch_delete();
            }
            // Undo/delete above may change pan availability. Freeze admission now:
            // later navigation shortcuts must not acquire a press after clipboard acted.
            // A refused press stays refused if Escape cancels its gesture later this frame;
            // only a fresh press may start minimap input after that cancellation.
            let minimap_owned = self.minimap_input_owned(ui.ctx());
            minimap_press = Some(minimap_owned);
            // Clipboard: the platform turns ⌘C/⌘X/⌘V into events; ⌘D duplicates in place.
            // Not mid-gesture or in a menu: a cut mid-drag would delete the card being dragged.
            // Nor while the API keys panel is open: text pasted for a key before its field has
            // focus must never become a node in the (autosaved) project.
            let busy = self.drag.is_some()
                || self.marquee.is_some()
                || self.wiring.is_some()
                || self.reordering.is_some()
                || minimap_owned
                || bulk_blocks
                || self.comments.drag.is_some()
                || self.comments.gesture_cancelled
                || egui::Popup::is_any_open(ui.ctx())
                || self.keys.open
                || self.drop_pointer_owned();
            // egui reports no ⌘V press when the clipboard holds no text, only its release (#319).
            #[cfg(not(target_arch = "wasm32"))]
            self.image_paste_keys(ui, t, !busy);
            let (copy, cut, pasted, duplicate) = ui.input_mut(|input| {
                // Yield before consuming shortcuts or cloning any pasted payload.
                if busy {
                    return (false, false, None, false);
                }
                let duplicate = input.consume_key(egui::Modifiers::COMMAND, egui::Key::D);
                let copy = input.events.iter().any(|e| matches!(e, egui::Event::Copy));
                let cut = input.events.iter().any(|e| matches!(e, egui::Event::Cut));
                let pasted = input.events.iter().rev().find_map(|event| match event {
                    egui::Event::Paste(text) if text.len() > clipboard::MAX_BYTES => Some(Err(())),
                    egui::Event::Paste(text) => {
                        #[cfg(test)]
                        PASTE_CLONES.with(|n| n.set(n.get() + 1));
                        Some(Ok(text.clone()))
                    }
                    _ => None,
                });
                (copy, cut, pasted, duplicate)
            });
            if copy || cut || duplicate {
                match self.selection_payload() {
                    Err(error) => self.notice = Some(error.message),
                    Ok(None) => {}
                    Ok(Some(payload)) => {
                        if duplicate {
                            self.duplicate_payload(payload, t);
                        } else if let Some(text) = clipboard::to_text(&payload) {
                            if cut {
                                self.cut_payload(ui.ctx(), payload, text);
                            } else {
                                ui.ctx().copy_text(text);
                                self.notice = None;
                            }
                        }
                    }
                }
            }
            match pasted {
                Some(Err(())) => self.notice = Some("The clipboard holds more than 16 MiB; nothing was pasted.".into()),
                Some(Ok(text)) => match self.paste_target(ui, t) {
                    Some(centre) => match clipboard::parse(&text) {
                        clipboard::Parsed::Nodes(payload) => self.paste(payload, centre),
                        clipboard::Parsed::Unreadable(why) => self.notice = Some(format!("Can't paste these nodes: {why}.")),
                        clipboard::Parsed::Text => self.paste_text(&text, centre),
                    },
                    None => self.notice = Some("There is no unobscured canvas space for this paste.".into()),
                },
                None => {}
            }
            if select_all {
                let membership = self.membership();
                if let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
                    self.selection.replace(canvas.nodes().keys().filter(|id| !membership.hidden.contains(id)).copied().collect());
                }
            }
            // Quick-add moved to the end of the frame (`quick_add` below), after every floating
            // Window has laid out, so the canvas can tell whether it really owns the pointer.
            // Its admission gate keeps this frame's `minimap_owned` freeze; see #267 and #361.
            // Menus and editors own Escape before canvas gestures; comments consume it above.
            if escape && !self.text_focused && !egui::Popup::is_any_open(ui.ctx()) {
                self.cancel_canvas_gesture();
            }
        }
        // Undo/redo, deletion, paste and quick-add above can change this frame's revision.
        // Restamp before either cached placeholder admission or marquee geometry is read.
        if self.marquee_geometry.prepare(&mut self.session, self.canvas, self.theme) {
            self.schemas = SchemaCache::default();
        }
        let popup_owned = egui::Popup::is_any_open(ui.ctx());
        // Taken before navigation too: a fit or pan from the overlay also moves the view.
        let view_before = self.view;
        let layout_blocks = if model_blocks { true } else { self.layout_overlay(ui) };
        #[cfg(not(target_arch = "wasm32"))]
        let file_owns_input = file_owns_input || self.files.owns_input();
        let overlay_blocks = self.navigation_overlay(ui, minimap_press)
            || model_blocks
            || bulk_blocks
            || search_blocks
            || layout_blocks
            || comments_blocks
            || tag_blocks
            || file_owns_input
            || file_menu_owns_input
            || drop_blocks;
        // Any popup owns canvas gestures, but its own card controls must still deliver edits.
        let canvas_blocks = overlay_blocks || popup_owned || file_owns_input || file_menu_owns_input || recovery_owns_input;
        let shift = ui.input(|i| i.modifiers.shift);
        let background = ui.interact(viewport, ui.id().with("canvas"), Sense::click_and_drag());
        if !canvas_blocks
            && background.drag_started_by(egui::PointerButton::Primary)
            && shift
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.marquee = Some(Marquee { start: self.view.to_world(viewport.min, origin), base: self.selection.clone(), cancelled: false });
        }
        let membership = self.membership();
        let collapsed = &membership.hidden;
        let over_card = ui.input(|i| i.pointer.hover_pos()).is_some_and(|pointer| {
            // navigation_overlay refreshed actual-instance geometry for this revision above.
            // The visible-card schema cache is refreshed later and may still describe an old size.
            // Cards hidden in a collapsed field aren't there to scroll.
            self.navigation.geometry.rects.iter().any(|(id, rect)| !collapsed.contains(id) && self.view.rect_to_screen(viewport.min, *rect).contains(pointer))
        });
        let home = if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Num0)) { self.home() } else { Pos2::ZERO };
        let allow_scroll_pan = !over_card || self.view.zoom < card::BODY_MIN_ZOOM;
        if !canvas_blocks {
            canvas::navigate(ui, &background, &mut self.view, home, &self.canvas_prefs, allow_scroll_pan, self.marquee.is_none());
        }
        // The palette and "Turn into" create at the world point they opened on; once the view
        // moves, close them.
        if self.view != view_before {
            self.palette = None;
            self.turn_into = None;
        }
        let painter = ui.painter_at(viewport);
        painter.rect_filled(viewport, 0.0, t.canvas_bg);
        canvas::paint_grid(&painter, viewport, &self.view, t);
        let mut intents = Vec::new();
        // Fields sit on the canvas layer, under the noodles and the card layers.
        let mut pan = self.fields_show(ui, viewport, canvas_blocks);
        let mut query = None;
        let mut split_request = None;
        let mut mask_request = None;
        let mut layers_request = None;
        let mut present_request = None;
        let mut reader_request = None;
        let mut timeline_request = None;
        // Element card menus offer saving to, or updating from, the library: know what it holds.
        self.element_library_listed();
        let mut element_request = false;
        let mut image_tool_request = None;
        let mut element_library_request = None;
        let mut technique_request = None;
        let mut style_request = None;
        let mut wire_command = None;
        let mut layout_mode = None;
        // The owning context menu may invoke layout; unrelated popups have no such route.
        let layout_context_ready = self.layout_base_available(ui.ctx());
        let mut comment_target = None;
        let mut split_after = None;
        let mut mask_after = None;
        let mut layers_after = None;
        let mut present_after = None;
        let mut reader_after = None;
        let mut timeline_after = None;
        let mut element_after = false;
        let mut image_tool_after = None;
        let mut element_library_after = None;
        let mut technique_after = None;
        let mut style_after = None;
        let mut tag_action = None;
        let mut field_action = None;
        let mut refresh = None;
        self.refresh_stale_badges();
        self.refresh_mask_badges();
        let tag_menu_allowed = !self.text_focused
            && !canvas::text_input_focused(ui.ctx())
            && self.drag.is_none()
            && self.marquee.is_none()
            && self.comments.drag.is_none()
            && !self.comments.gesture_cancelled
            && !self.navigation.pointer_owned
            && self.navigation.drag.is_none()
            && self.palette.is_none()
            && self.turn_into.is_none()
            && self.reordering.is_none()
            && self.wiring.is_none()
            && ui.input(|i| i.raw.hovered_files.is_empty() && i.raw.dropped_files.is_empty());
        // Cards the viewers' capture buttons asked for last frame.
        self.take_mesh_captures();
        // After `fields_show`, which may have collapsed or expanded a field this frame.
        let membership = self.membership();
        if let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
            let kinds = self.session.kinds();
            self.editors.retain(|id, _| canvas.nodes().contains_key(id));
            self.videos.retain(|id| canvas.nodes().contains_key(&id));
            self.audios.retain(|id| canvas.nodes().contains_key(&id));
            self.mesh_views.retain(|id, _| canvas.nodes().contains_key(id));
            self.schemas.retain(|id| canvas.nodes().contains_key(id));
            self.selection.retain_existing(canvas);
            // Selected cards draw last, on top.
            let selected = self.selection.clone();
            self.painted.clear();
            let prompt_wired: BTreeSet<NodeId> =
                canvas.edges().values().filter(|edge| edge.to.port == nodecraft_nodes::generation::PROMPT).map(|edge| edge.to.node).collect();
            let element_names = elements::names(canvas);
            self.mention_cache.refresh(self.session.revision(), self.canvas, canvas, kinds);
            let mention_cards = &self.mention_cache.cards;
            let mention_blocked = &self.mention_cache.blocked;
            let hidden = &membership.hidden;
            let field_shift = self.field_shift();
            for node in
                canvas.nodes().values().filter(|node| !selected.contains(node.id)).chain(canvas.nodes().values().filter(|node| selected.contains(node.id)))
            {
                if hidden.contains(&node.id) {
                    if let Some(editor) = self.editors.get_mut(&node.id) {
                        editor.mark_hidden();
                    }
                    continue;
                }
                let preview = self.drag.as_ref().filter(|drag| drag.id == node.id).map(|drag| drag.pos).or_else(|| Self::field_preview(field_shift, node));
                // Cull conservatively before resolving potentially large instance schemas.
                let position = preview.unwrap_or(node.pos);
                let bounds = Rect::from_min_size(Pos2::new(position.x, position.y), Vec2::new(t.card_width, t.card_max_height + t.card_preview_height));
                if !self.view.rect_to_screen(viewport.min, bounds).expand(card::cull_margin(t, self.view.zoom)).intersects(viewport) {
                    if let Some(editor) = self.editors.get_mut(&node.id) {
                        editor.mark_hidden();
                    }
                    continue;
                }
                // Only cards on screen at a readable zoom ask for their image (#386).
                let images = if self.view.zoom >= card::BODY_MIN_ZOOM {
                    // A slider dragging shows its preview's images, from the preview's own store (#88).
                    let preview = self.live.preview(self.canvas, node.id);
                    let (rows, count) = match (node.params.get("asset"), preview) {
                        (_, Some((result, _))) => nodecraft_engine::split::result_rows(result),
                        // A PDF source shows its first page (#113).
                        (Some(nodecraft_types::Value::Media(pdf)), None) if node.kind == nodecraft_nodes::document::SOURCE => (vec![vec![pdf]], 1),
                        _ => nodecraft_engine::split::image_rows(node),
                    };
                    // A tile's edge: the card's width shared by the widest row (#519).
                    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1) as f32;
                    let edge = t.card_width / columns * self.view.zoom * ui.ctx().pixels_per_point();
                    let assets = preview.map_or(self.session.assets(), |(_, staged)| staged);
                    let previews = &mut self.previews;
                    let rows = rows
                        .iter()
                        .map(|row| row.iter().map(|asset| previews.request_asset(ui.ctx(), asset.hash, edge, || assets.preview_read(asset))).collect())
                        .collect();
                    // Each step of a drag makes new images: the last ones stay until these decode.
                    let rows = if preview.is_some() { self.live.steady(node.id, rows) } else { rows };
                    (rows, count)
                } else {
                    (Vec::new(), 0)
                };
                // Videos likewise; the player is view state, kept per card (#105).
                let video =
                    (self.view.zoom >= card::BODY_MIN_ZOOM).then(|| crate::video::video_of(node)).flatten().filter(|_| images.0.is_empty()).map(|asset| {
                        let edge = t.card_width * self.view.zoom * ui.ctx().pixels_per_point();
                        let assets = self.session.assets();
                        self.videos.view(ui.ctx(), node.id, asset, edge, || assets.preview_read(asset))
                    });
                // And sounds; which card plays, and where, is view state too (#106).
                let audio = (self.view.zoom >= card::BODY_MIN_ZOOM)
                    .then(|| crate::audio::audio_of(node))
                    .flatten()
                    .filter(|_| images.0.is_empty() && video.is_none())
                    .map(|asset| {
                        let assets = self.session.assets();
                        self.audios.view(ui.ctx(), node.id, asset, || assets.preview_read(asset))
                    });
                let mesh = match crate::mesh_view::mesh_of(node) {
                    Some(asset) if self.view.zoom >= card::BODY_MIN_ZOOM => {
                        let assets = self.session.assets();
                        let preview = self.meshes.request(ui.ctx(), asset, || assets.preview_read(asset));
                        Some(crate::mesh_view::CardMesh { preview, view: self.mesh_views.entry(node.id).or_default(), gpu: self.mesh_gpu })
                    }
                    _ => None,
                };
                let rejected = self.marquee.is_some() && self.marquee_geometry.placeholder(node.id);
                let schema = if rejected { None } else { cached_schema(&mut self.schemas, &self.session, node, self.cache_clock) };
                let descriptor = schema.map(|cached| cached.descriptor.as_ref());
                let outdated = plugin_cards::outdated(&self.session, node);
                let editor = self.editors.entry(node.id).or_default();
                // Cards use their parent's max_rect origin as the canvas origin. A layout
                // scope would advance that origin between cards; an independent disabled
                // child keeps the original viewport and leaves ordinary rendering untouched.
                let mut disabled_ui =
                    (file_owns_input || file_menu_owns_input).then(|| ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).disabled()));
                let card_ui = match &mut disabled_ui {
                    Some(child) => child,
                    None => &mut *ui,
                };
                let result = card::show(
                    card_ui,
                    &self.view,
                    node,
                    card::CardView {
                        descriptor,
                        visible: schema.map(|cached| &cached.visible),
                        enabled: schema.and_then(|cached| cached.enabled.as_ref()),
                        preview_pos: preview,
                        images,
                        grid: self.split_shown.filter(|(id, ..)| *id == node.id).map(|(_, rows, columns)| {
                            let full = nodecraft_engine::split::image_of(node).and_then(|asset| Some([asset.meta.width?, asset.meta.height?]));
                            (rows, columns, full)
                        }),
                        roles: schema.map(|cached| &cached.roles),
                        listed: schema.map(|cached| &cached.listed),
                        lists: self.session.option_lists(),
                        prompt_wired: prompt_wired.contains(&node.id),
                        prompt_overlay: self.prompt_overlays,
                        elements: &element_names,
                        mentions: mention_cards,
                        mention_blocked: mention_blocked.get(&node.id).map(String::as_str),
                        partial: self.runs.as_ref().and_then(|runs| runs.partial(self.canvas, node.id)),
                        video,
                        audio,
                        mesh,
                        outdated,
                        variations: card::generate::Variations {
                            cards: canvas.variation_cards(),
                            alike: nodecraft_exec::variations::requested(node) > 1
                                && self
                                    .session
                                    .generation_catalog()
                                    .is_ok_and(|catalog| !nodecraft_engine::variation_cmds::pinned_seeds(catalog, node).is_empty()),
                        },
                    },
                    editor,
                    self.selection.contains(node.id),
                    t,
                );
                if let Some(result) = result {
                    let r = result.response;
                    self.painted.insert(node.id, r.rect);
                    let tag_painter = egui::Painter::new(ui.ctx().clone(), r.layer_id, tag_world_clip(&self.view, viewport, ui.clip_rect()));
                    if let Some(tag) = node.tag.and_then(|id| canvas.tags().get(&id)) {
                        let marker = Rect::from_min_size(
                            r.rect.min + Vec2::splat(t.navigation_gap),
                            Vec2::new(t.tag_marker_width, t.tag_row_height.min(r.rect.height())),
                        );
                        tag_painter.rect_filled(marker, t.tag_marker_width, t.tag_color(tag.color.as_deref()));
                        ui.interact(self.view.rect_to_screen(viewport.min, marker), ui.id().with(("tag_marker", node.id)), Sense::hover())
                            .on_hover_text(format!("Tag: {}", crate::tags::label(&tag.title)));
                    }
                    if let Some(runs) = &self.runs
                        && let Some((label, color, why)) = queue_panel::card_status(runs, self.canvas, node.id, t)
                    {
                        let pos = r.rect.left_bottom() + Vec2::new(0.0, t.control_gap);
                        let text = tag_painter.text(pos, egui::Align2::LEFT_TOP, &label, egui::FontId::proportional(t.label_size), color);
                        let hover = ui.interact(self.view.rect_to_screen(viewport.min, text), ui.id().with(("run_status", node.id)), Sense::hover());
                        hover.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &label));
                        if let Some(why) = why {
                            hover.on_hover_text(why);
                        }
                    }
                    // A card being refreshed keeps its stale result until the run records, but
                    // offers no second refresh meanwhile.
                    let running = self.runs.as_ref().is_some_and(|runs| runs.is_running(self.canvas, node.id));
                    // A mask whose image drifted shows that instead: refreshing it would paint the
                    // old strokes onto the new image.
                    if let Some(drift) = self.mask_badges.as_ref().and_then(|badges| badges.masks.get(&node.id).copied()) {
                        if mask_badges::badge(ui, &tag_painter, &self.view, viewport, r.rect, node.id, drift, t) && !canvas_blocks {
                            mask_request = Some(node.id);
                        }
                    } else if !running
                        && self.stale_badges.as_ref().is_some_and(|badges| badges.nodes.contains(&node.id))
                        && stale_badges::badge(ui, &tag_painter, &self.view, viewport, r.rect, node.id, t)
                        && !canvas_blocks
                    {
                        refresh = Some(node.id);
                    }
                    if self.tag_filter.active() {
                        let matches = self.tag_filter.matches(node.id);
                        tag_painter.rect_stroke(
                            r.rect.expand(t.tag_marker_width),
                            t.card_radius,
                            egui::Stroke::new(if matches { t.handle_stroke * 2.0 } else { t.handle_stroke }, if matches { t.accent } else { t.panel_stroke }),
                            egui::StrokeKind::Outside,
                        );
                    }
                    if !canvas_blocks && r.clicked() {
                        if shift {
                            self.selection.toggle(node.id);
                        } else {
                            self.selection.only(node.id);
                        }
                    } else if !canvas_blocks
                        && (r.drag_started_by(egui::PointerButton::Primary) || r.secondary_clicked() || r.long_touched())
                        && !self.selection.contains(node.id)
                    {
                        // Grabbing or right-clicking a selected card keeps the rest of the selection.
                        if shift {
                            self.selection.add(node.id);
                        } else {
                            self.selection.only(node.id);
                        }
                    }
                    if !model_blocks
                        && (!overlay_blocks || egui::Popup::is_any_open(ui.ctx()))
                        && (!self.text_focused && !canvas::text_input_focused(ui.ctx()) || egui::Popup::is_any_open(ui.ctx()))
                    {
                        r.context_menu(|ui| {
                            let owner = egui::Popup::context_menu(&r).get_id();
                            let owned = egui::Popup::is_id_open(ui.ctx(), owner);
                            ui.label("Format selection");
                            ui.add_enabled_ui(layout_context_ready && owned, |ui| {
                                if ui.button("Grid").clicked() {
                                    layout_mode = Some(("grid", owner));
                                    ui.close();
                                }
                                if ui.button("Dependency order").clicked() {
                                    layout_mode = Some(("layered", owner));
                                    ui.close();
                                }
                            });
                            ui.separator();
                            if ui
                                .add_enabled(owned && !self.text_focused && !canvas::text_input_focused(ui.ctx()), egui::Button::new("Comment on this node"))
                                .clicked()
                            {
                                comment_target = Some((node.id, owner));
                                ui.close();
                            }
                            ui.add_enabled_ui(tag_menu_allowed, |ui| {
                                if let Some(action) = tags::tag_menu(ui, canvas, &self.selection, t, &self.tag_filter.presentation) {
                                    tag_action = Some(action);
                                }
                                if let Some(action) = fields::card_menu(ui, canvas, &self.selection, t) {
                                    field_action = Some(action);
                                }
                            });
                            if ui.button("Select upstream").clicked() {
                                query = Some(SelectQuery::Upstream);
                            }
                            if ui.button("Select downstream").clicked() {
                                query = Some(SelectQuery::Downstream);
                            }
                            let label = descriptor.map_or(node.kind.as_str(), |d| d.label.as_str());
                            if ui.button(format!("Select all {label} nodes")).clicked() {
                                query = Some(SelectQuery::Kind(node.kind.clone()));
                            }
                            if node.kind != nodecraft_nodes::mask::ID
                                && let Some(from) = image_tools::output(node)
                                && let Ok(entries) = nodecraft_engine::image_tool_cmds::menu(canvas, self.session.generation_catalog().ok(), &from)
                            {
                                let has_image = nodecraft_engine::split::image_of(node).is_some();
                                ui.menu_button("Image tools", |ui| {
                                    for entry in &entries {
                                        if entry.tool != nodecraft_engine::image_tool_cmds::Tool::SplitGrid {
                                            if let Some(request) = image_tools::button(ui, entry, node, &from, has_image) {
                                                image_tool_request = Some(request);
                                            }
                                            continue;
                                        }
                                        if !has_image || entry.reason.is_some() {
                                            ui.add_enabled(false, egui::Button::new(entry.tool.label()))
                                                .on_disabled_hover_text(entry.reason.as_deref().unwrap_or("Run or load the image first"));
                                            continue;
                                        }
                                        ui.menu_button(entry.tool.label(), |ui| {
                                            let (rows, columns) = &mut self.split_size;
                                            let side = 1..=nodecraft_engine::split::MAX_SIDE;
                                            ui.add(egui::DragValue::new(rows).range(side.clone()).prefix("Rows: "));
                                            ui.add(egui::DragValue::new(columns).range(side).prefix("Columns: "));
                                            // The card draws these lines over its image while the menu is open.
                                            self.split_seen = Some((node.id, *rows, *columns));
                                            if self.split_shown != self.split_seen {
                                                ui.ctx().request_repaint();
                                            }
                                            let tiles = rows.saturating_mul(*columns);
                                            let fits = tiles <= nodecraft_engine::split::MAX_TILES;
                                            let button = ui.add_enabled(fits, egui::Button::new(format!("Split into {tiles} images")));
                                            let button = if fits {
                                                button
                                            } else {
                                                button.on_disabled_hover_text(format!("At most {} tiles", nodecraft_engine::split::MAX_TILES))
                                            };
                                            if button.clicked() {
                                                split_request = Some((node.id, *rows, *columns));
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                            }
                            if node.kind == nodecraft_nodes::document::SOURCE && ui.button("Read PDF…").clicked() {
                                reader_request = Some(node.id);
                                ui.close();
                            }
                            if reader::reads_pages(node)
                                && ui
                                    .add_enabled(reader::source_of(canvas, node.id).is_some(), egui::Button::new("Choose pages…"))
                                    .on_disabled_hover_text("Connect a PDF first")
                                    .clicked()
                            {
                                reader_request = Some(node.id);
                                ui.close();
                            }
                            if node.kind == nodecraft_nodes::layers::ID && ui.button("Edit layers…").clicked() {
                                layers_request = Some(node.id);
                                ui.close();
                            }
                            if node.kind == nodecraft_nodes::deck::ID {
                                if ui.button("Edit slides…").clicked() {
                                    layers_request = Some(node.id);
                                    ui.close();
                                }
                                if ui.button("Present").on_hover_text("Show the slides full-window; Esc leaves").clicked() {
                                    present_request = Some(Some(node.id));
                                    ui.close();
                                }
                            } else if ui.button("Present selection").on_hover_text("Make a deck with a slide per selected card, and show it").clicked() {
                                present_request = Some(None);
                                ui.close();
                            }
                            if node.kind == nodecraft_nodes::timeline::ID && ui.button("Edit timeline…").clicked() {
                                timeline_request = Some(node.id);
                                ui.close();
                            }
                            if node.kind == nodecraft_nodes::mask::ID && ui.button("Paint mask…").clicked() {
                                mask_request = Some(node.id);
                                ui.close();
                            }
                            if !elements::sources(canvas, self.selection.nodes()).is_empty() {
                                let r = ui.button("Make Element from selection").on_hover_text("A reusable subject prompts can mention by #name");
                                if r.clicked() {
                                    element_request = true;
                                    ui.close();
                                }
                            }
                            if node.kind == nodecraft_nodes::element::ID
                                && let Some(action) = element_library_view::menu(ui, &self.element_library, node)
                            {
                                element_library_request = Some(action);
                            }
                            if let Some(action) = styles::menu(ui, canvas, self.session.generation_catalog().ok(), node) {
                                style_request = Some(action);
                            }
                            if let Some(action) = technique_menu::menu(ui, self.session.document(), node) {
                                technique_request = Some(action);
                            }
                        });
                    }
                    if !canvas_blocks && r.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(serial) = self.next_gesture.checked_add(1) {
                            self.next_gesture = serial;
                            self.drag = Some(Drag { id: node.id, gesture: format!("canvas-drag-{serial}"), pos: node.pos });
                        } else {
                            self.notice = Some("Too many drag gestures; reopen the project to continue.".into());
                            self.drag = None;
                        }
                    }
                    if !canvas_blocks && (r.dragged_by(egui::PointerButton::Middle) || r.dragged_by(egui::PointerButton::Secondary)) {
                        pan += r.drag_delta() * self.view.zoom;
                    }
                    for (side, port, r) in &result.ports {
                        if overlay_blocks {
                            continue;
                        }
                        if r.dragged_by(egui::PointerButton::Middle) || r.dragged_by(egui::PointerButton::Secondary) {
                            pan += r.drag_delta() * self.view.zoom;
                        }
                        if !r.drag_started_by(egui::PointerButton::Primary) {
                            continue;
                        }
                        self.wiring = match side {
                            Side::Output => node.outputs.iter().find(|p| p.name == *port).map(|p| Wiring {
                                from: Endpoint { node: node.id, port: port.clone() },
                                port_type: p.port_type,
                                moving: None,
                            }),
                            // Grabbing a connected input picks up its last noodle to reroute it.
                            Side::Input => noodle::into_port(canvas, &Endpoint { node: node.id, port: port.clone() }).last().and_then(|edge| {
                                let source = canvas.nodes().get(&edge.from.node)?.outputs.iter().find(|p| p.name == edge.from.port)?;
                                Some(Wiring { from: edge.from.clone(), port_type: source.port_type, moving: Some(edge.id) })
                            }),
                        };
                    }
                    // A Choice popup can close while producing its parameter intent over the
                    // overlay. Preserve its ownership from frame start, without admitting moves.
                    intents.extend(
                        result
                            .intents
                            .into_iter()
                            .filter(|intent| {
                                !model_blocks
                                    && !file_owns_input
                                    && !file_menu_owns_input
                                    && (!overlay_blocks || self.text_focused || (popup_owned && matches!(intent, Intent::Parameter { .. })))
                                    && (!canvas_blocks || !matches!(intent, Intent::Move(_)))
                            })
                            .map(|intent| (node.id, intent)),
                    );
                }
            }
            // Noodles follow this frame's painted cards (drag previews, resolved heights). They are
            // painted on the canvas layer, which stays under the card layers.
            let curves = if hidden.is_empty() {
                curves(canvas, &self.painted, kinds, t)
            } else {
                let mut geometry = self.painted.clone();
                geometry.extend(fields::stand_ins(canvas, hidden, t, self.view.zoom, field_shift));
                let mut curves = curves(canvas, &geometry, kinds, t);
                curves.retain(|(id, _, _)| canvas.edges().get(id).is_none_or(|edge| !fields::inside_collapsed(canvas, hidden, edge.from.node, edge.to.node)));
                curves
            };
            if !overlay_blocks && !file_owns_input && !file_menu_owns_input && background.clicked() {
                let hit = ui.input(|i| i.pointer.interact_pos()).and_then(|p| {
                    let world = self.view.to_world(viewport.min, p);
                    let reach = NOODLE_HIT / self.view.zoom;
                    curves.iter().map(|(id, curve, _)| (*id, noodle::distance(curve, world))).filter(|(_, d)| *d <= reach).min_by(|a, b| a.1.total_cmp(&b.1))
                });
                match (hit, shift) {
                    (Some((id, _)), true) => self.selection.toggle_edge(id),
                    (Some((id, _)), false) => self.selection.only_edge(id),
                    (None, false) => {
                        self.selection.clear();
                        self.fields.selected = None;
                    }
                    (None, true) => {}
                }
            }
            // Double-click empty canvas (not a noodle) for the quick-add palette.
            if !canvas_blocks
                && background.double_clicked()
                && let Some(p) = ui.input(|i| i.pointer.interact_pos())
            {
                let world = self.view.to_world(viewport.min, p);
                let reach = NOODLE_HIT / self.view.zoom;
                if !curves.iter().any(|(_, curve, _)| noodle::distance(curve, world) <= reach) {
                    self.palette = Some(Palette::new(world, p));
                    self.turn_into = None;
                }
            }
            // A noodle whose source node or picked-up edge vanished mid-drag (Delete, undo) is dropped.
            if self.wiring.as_ref().is_some_and(|w| !canvas.nodes().contains_key(&w.from.node) || w.moving.is_some_and(|id| !canvas.edges().contains_key(&id)))
            {
                self.wiring = None;
            }
            let moving = self.wiring.as_ref().and_then(|w| w.moving);
            // Widened by the halo so noodles just outside the edge still paint their stroke.
            let visible = self.view.visible_world(viewport).expand((t.wire_width_selected + t.wire_halo) / self.view.zoom);
            // Diagnostic-only: do not read options or flatten again while capture is off.
            let visibility = self.session.perf().enabled().then(|| {
                // Context accessors must not be nested inside its options read lock.
                let pixels_per_point = ui.ctx().pixels_per_point();
                let clip = painter.clip_rect();
                ui.ctx().tessellation_options(|options| metrics::visibility::Visibility::new(clip, pixels_per_point, options))
            });
            let mut visible_edges = if matches!(visibility, Some(None)) { None } else { Some(0usize) };
            for (id, curve, modality) in &curves {
                // The curve lies inside its control points' hull; skip ones entirely off screen.
                if Some(*id) == moving || !Rect::from_points(curve).intersects(visible) {
                    continue;
                }
                let points = curve.map(|p| self.view.to_screen(viewport.min, p));
                if let Some(Some(visibility)) = &visibility {
                    let widest = if self.selection.contains_edge(*id) { t.wire_width_selected + t.wire_halo } else { t.wire_width };
                    visible_edges = visible_edges.and_then(|count| visibility.intersects(points, widest).map(|hit| count.saturating_add(usize::from(hit))));
                }
                if self.selection.contains_edge(*id) {
                    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                        points,
                        false,
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(t.wire_width_selected + t.wire_halo, t.card_stroke_selected),
                    ));
                }
                let width = if self.selection.contains_edge(*id) { t.wire_width_selected } else { t.wire_width };
                painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    points,
                    false,
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(width, t.wire(*modality)),
                ));
            }
            if visibility.is_some() {
                self.frame_workload = Some(nodecraft_engine::perf::Workload {
                    visible_nodes: self.painted.len(),
                    total_nodes: canvas.nodes().len(),
                    visible_edges,
                    total_edges: canvas.edges().len(),
                    visibility_unavailable: visible_edges.is_none().then_some("invalid_curve_geometry_or_options"),
                });
            }
            // Badges show where their numbers can be read; dragging one reorders the input.
            if !overlay_blocks && self.view.zoom >= card::BODY_MIN_ZOOM {
                let badges = order::Badges { canvas_id: self.canvas, view: &self.view, reordering: &mut self.reordering };
                let (command, badge_pan) = badges.show(ui, canvas, kinds, &curves, viewport, t);
                wire_command = command;
                pan += badge_pan;
            } else {
                self.reordering = None;
            }
            if let Some(wiring) = &self.wiring
                && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            {
                let (command, notice, turn_into) = self.wire(ui, canvas, wiring, pointer, t);
                wire_command = command;
                if turn_into.is_some() {
                    self.palette = None;
                    self.turn_into = turn_into;
                }
                if notice.is_some() {
                    self.notice = notice;
                }
            }
            match query {
                Some(SelectQuery::Upstream) => self.selection.replace(selection::upstream(canvas, self.selection.nodes())),
                Some(SelectQuery::Downstream) => self.selection.replace(selection::downstream(canvas, self.selection.nodes())),
                Some(SelectQuery::Kind(kind)) => self.selection.replace(selection::of_kind(canvas, &kind)),
                None => {}
            }
            split_after = split_request;
            mask_after = mask_request;
            layers_after = layers_request;
            present_after = present_request;
            reader_after = reader_request;
            timeline_after = timeline_request;
            element_after = element_request;
            image_tool_after = image_tool_request;
            element_library_after = element_library_request;
            technique_after = technique_request;
            style_after = style_request;
            if let Some(marquee) = &mut self.marquee
                && !marquee.cancelled
                && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            {
                // Ids removed mid-drag (e.g. by undo) must not come back from the base.
                marquee.base.retain_existing(canvas);
                let world = Rect::from_two_pos(marquee.start, self.view.to_world(viewport.min, pointer));
                self.selection = marquee.base.clone();
                self.selection.extend(
                    selection::in_box(canvas, world, |node| box_geometry(&self.painted, &self.session, &mut self.marquee_geometry, node, world, t)),
                    curves.iter().filter(|(_, curve, _)| noodle::touches(curve, world)).map(|(id, _, _)| *id),
                );
                self.selection.remove_nodes(hidden);
                // Above the card layers, which are drawn over the background painter.
                ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("marquee"))).with_clip_rect(viewport).rect(
                    self.view.rect_to_screen(viewport.min, world),
                    0.0,
                    t.marquee_fill,
                    t.marquee_stroke(),
                    egui::StrokeKind::Inside,
                );
            }
        } else {
            self.selection.clear();
        }
        // Drawn while a popup is open too, so its own aspect menu stays alive; it acts only
        // when nothing else owns the frame.
        let toolbar_live = !overlay_blocks && !file_owns_input && !file_menu_owns_input && !recovery_owns_input;
        intents.extend(self.generation_toolbar(ui, viewport, toolbar_live));
        self.image_viewer(ui.ctx());
        self.pdf_reader(ui.ctx());
        if let Some(action) = tag_action {
            if let Err(error) = self.apply_tag_action(action) {
                self.notice = Some(error.message);
            }
            ui.ctx().request_repaint();
        }
        if let Some(action) = field_action {
            self.field_card_action(action);
            ui.ctx().request_repaint();
        }
        // Cards were painted before this frame's selection changes; show them without waiting for input.
        if self.selection != selected_before {
            ui.ctx().request_repaint();
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.wiring = None;
        }
        if let Some((node, rows, columns)) = split_after {
            self.split_grid(node, rows, columns, t);
        }
        if let Some(node) = reader_after {
            self.open_reader(node);
        }
        if let Some(node) = layers_after {
            let deck =
                self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&node)).is_some_and(|n| n.kind == nodecraft_nodes::deck::ID);
            if deck { self.open_deck_editor(node) } else { self.open_layer_editor(node) }
        }
        match present_after {
            Some(Some(node)) => self.open_presenter(node),
            Some(None) => self.present_selection(),
            None => {}
        }
        if let Some(node) = timeline_after {
            self.open_timeline_editor(node);
        }
        if let Some(node) = mask_after {
            self.mask_editor = Some(mask_editor::MaskEditor::new(self.canvas, node));
        }
        if element_after {
            self.make_element();
        }
        if let Some(request) = image_tool_after {
            self.image_tool(request);
        }
        if let Some(action) = element_library_after {
            self.element_library_action(action);
        }
        if let Some(action) = technique_after {
            self.technique_action(action);
        }
        if let Some(action) = style_after {
            self.style_action(action);
        }
        if let Some((command, params)) = wire_command {
            match self.session.dispatch(command, &params) {
                Ok(_) => self.notice = None,
                Err(error) => self.notice = Some(error.message),
            }
        }
        if let Some((mode, owner)) = layout_mode {
            self.apply_layout_menu(ui.ctx(), mode, owner);
        }
        if let Some((node, owner)) = comment_target {
            self.compose_on_node(ui.ctx(), node, Some(owner));
        }
        if let Some(id) = intents.iter().find_map(|(id, intent)| matches!(intent, Intent::Model).then_some(*id))
            && let Err(error) = self.open_model_picker(self.canvas, vec![id], self.session.revision())
        {
            self.notice = Some(error.message);
        }
        for (id, intent) in intents {
            // A model-opening click can make a text draft lose focus in this same pass.
            // Preserve its draft; never auto-commit it before constructing the preview.
            if self.model_frame_owned {
                continue;
            }
            let (command, params, editing) = match intent {
                Intent::Move(pos) => {
                    if let Some(drag) = &mut self.drag
                        && drag.id == id
                    {
                        drag.pos = pos;
                    }
                    continue;
                }
                Intent::Title(title) => ("node.title", json!({"canvas":self.canvas,"id":id,"title":title}), Some(EditTarget::Title)),
                Intent::Parameter { name, value } => {
                    ("node.param_set", json!({"canvas":self.canvas,"id":id,"name":name,"value":value}), Some(EditTarget::Parameter(name)))
                }
                Intent::History(intent) => match self.history_command(id, intent, t) {
                    Some((command, params)) => (command, params, None),
                    None => continue,
                },
                Intent::Variations(count) => ("node.variations_set", json!({"canvas":self.canvas,"id":id,"count":count}), None),
                Intent::VariationCards(cards) => ("canvas.set_variation_cards", json!({"canvas":self.canvas,"cards":cards}), None),
                Intent::Model => continue,
                Intent::Migrate => {
                    self.migrate_card(id);
                    continue;
                }
                Intent::Video(intent) => {
                    self.videos.act(ui.ctx(), id, intent);
                    continue;
                }
                Intent::Audio(intent) => {
                    self.audios.act(ui.ctx(), id, intent);
                    continue;
                }
                Intent::KeepPartial | Intent::DiscardPartial => {
                    self.partial_intent(id, matches!(intent, Intent::KeepPartial));
                    continue;
                }
                // After this pass's edits above it, so a prompt submitted with Cmd+Enter is quoted.
                Intent::Generate => {
                    if let Err(error) = self.request_generate(id) {
                        self.notice = Some(error.message);
                    }
                    continue;
                }
            };
            match self.session.dispatch(command, &params) {
                Ok(_) => {
                    self.notice = None;
                    if let Some(target) = editing
                        && let Some(editor) = self.editors.get_mut(&id)
                    {
                        editor.accepted(&target);
                    }
                }
                Err(error) => {
                    self.notice = Some(error.message.clone());
                    if let Some(target) = editing
                        && let Some(editor) = self.editors.get_mut(&id)
                    {
                        editor.failed(target, error.message);
                    }
                }
            }
        }
        // After this pass's edits, like Generate, so the quote sees them.
        if let Some(id) = refresh
            && let Err(error) = self.request_run(id, generate::RunKind::Refresh)
        {
            self.notice = Some(error.message);
        }
        if !ui.input(|i| i.pointer.primary_down())
            && let Some(drag) = self.drag.take()
            && let Err(error) = self.session.dispatch("node.move", &json!({"canvas":self.canvas,"id":drag.id,"pos":drag.pos,"gesture":drag.gesture}))
        {
            self.notice = Some(error.message);
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.marquee = None;
            self.marquee_geometry.clear();
        }
        self.view.pan_by(pan);
        // All canvas gestures (including late card/port pan) have now been admitted.
        // Close before the palette can consume the first Escape meant for that gesture.
        // The `self.view != view_before` arm is not covered by the earlier close: cards and order
        // badges only collect their pan, which `view.pan_by` applies just above, after that check
        // ran. A reorder drag holds neither `drag` nor `marquee` nor `wiring`, so it needs its own
        // arm too (#267).
        if self.drag.is_some() || self.marquee.is_some() || self.wiring.is_some() || self.reordering.is_some() || self.view != view_before {
            self.palette = None;
        }
        if !self.model_frame_owned {
            self.drop_files(ui, viewport, t);
        }
        if let Some(mut open) = self.palette.take() {
            let entries = self.palette_entries();
            match open.show(ui.ctx(), &entries, t) {
                palette::Outcome::Open => self.palette = Some(open),
                palette::Outcome::Closed => {}
                palette::Outcome::Chosen(palette::Target::Kind(kind)) => self.create_node(&kind, open.world),
                palette::Outcome::Chosen(palette::Target::Command("perf.report")) => match self.session.dispatch("perf.report", &json!({})) {
                    // The report is for reading elsewhere: put it on the clipboard.
                    Ok(report) => {
                        ui.ctx().copy_text(serde_json::to_string_pretty(&report).unwrap_or_default());
                        self.notice = Some("Performance report copied to the clipboard.".into());
                    }
                    Err(error) => self.notice = Some(error.message),
                },
                palette::Outcome::Chosen(palette::Target::Command(id)) => {
                    #[cfg(not(target_arch = "wasm32"))]
                    if matches!(id, "file.open" | "file.save" | "file.save_as") {
                        self.begin_file(id, ui.ctx());
                    } else {
                        self.dispatch_palette_command(id);
                    }
                    #[cfg(target_arch = "wasm32")]
                    if matches!(id, "file.open" | "file.save" | "file.save_as") {
                        self.web_file(id, ui.ctx());
                    } else {
                        self.dispatch_palette_command(id);
                    }
                }
            }
        }
        if let Some(mut open) = self.turn_into.take() {
            match open.show(ui.ctx(), t) {
                turn_into::Outcome::Open => self.turn_into = Some(open),
                turn_into::Outcome::Closed => {}
                turn_into::Outcome::Chosen(kind) => self.create_connected(&kind, &open),
            }
        }
        // All document-changing frame effects precede the last-check stamp test and paint.
        self.show_notices(ui, viewport, t);
        #[cfg(not(target_arch = "wasm32"))]
        self.show_update_banner(ui, viewport, t);
        self.navigation.finish_frame(ui.ctx());
        self.tag_focused = self.tag_filter.open && ui.ctx().memory(|m| m.focused()).is_some_and(|id| self.tag_focus_ids.contains(&id));
        self.search_focused = ui.ctx().memory(|m| m.focused()) == Some(self.search_input_id());
        self.bulk.finish_frame(ui.ctx());
        self.text_focused = canvas::text_input_focused(ui.ctx());
        self.last_text_focus = self.text_focused.then(|| ui.ctx().memory(|m| m.focused())).flatten();
        self.keys_button(ui, viewport);
        #[cfg(not(target_arch = "wasm32"))]
        self.file_button(ui, t);
        #[cfg(target_arch = "wasm32")]
        self.web_file_button(ui, t);
        let changes = self.keys.show(ui.ctx(), &mut self.session, &self.key_accounts, t);
        self.save_accounts(changes);
        self.plugin_panel.show(ui.ctx(), &mut self.session, t);
        self.queue_panel(ui.ctx(), t);
        self.share_panel(ui.ctx(), t);
        #[cfg(not(target_arch = "wasm32"))]
        self.ownership_panel(ui.ctx(), t);
        self.sheets_panel(ui.ctx(), t);
        self.techniques_panel(ui.ctx(), t);
        self.elements_panel(ui.ctx(), t);
        // Floating UI must lay out before final quick-add admission, including a Window's first
        // sizing pass. Keep Keys rendering/polling exactly once, in its existing place.
        // `minimap_press` is this frame's frozen minimap admission (#361): held minimap input owns
        // mutation keys, and a quick-add creates a node, so it carries into the gate below.
        // `model_blocks` and `bulk_blocks` are this frame's model-swap (#472) and bulk-panel (#473)
        // admissions. Main's inline quick-add ran inside that canvas-keys guard, so the relocated
        // version carries both terms forward (same precedent as #460's drop guard).
        // A menu that was open when the frame began keeps owning its own key even after it has
        // closed: its pick and quick-add's press arrive in the same raw-input frame (#267).
        if !palette_owned_input && !turn_into_owned_input {
            self.quick_add(ui, viewport, minimap_press.unwrap_or_default(), model_blocks, bulk_blocks);
        }
        ui.ctx().options_mut(|o| o.input_options.surrender_focus_on = surrender_focus_on);
    }

    /// Hosts embedding floating Windows must lay them out before this app's final admission.
    /// The framework top-layer query alone misses a new Window's noninteractable sizing pass;
    /// current AreaState rectangles cover that pass too, including transformed title/chrome.
    fn canvas_owns_quick_add_pointer(&self, ui: &egui::Ui) -> bool {
        // No viewport test here on purpose: main has always created at the viewport centre when
        // the pointer sits outside the canvas (`minimap_drag_owns_quick_add_keys_until_pointer
        // _release`), and a point outside the canvas is not over a floating window either. The
        // area scan below still refuses a window that reaches past the canvas edge.
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else { return true };
        let canvas = ui.layer_id();
        let canvas_family = |layer: egui::LayerId| {
            layer == canvas
                // Canvas-owned navigation is not a floating Window, so quick-add over a visible
                // minimap stays available; a *held* minimap gesture is refused by the busy gate.
                || layer == egui::LayerId::new(egui::Order::Foreground, ui.id().with("navigation_overlay"))
                || (layer.order == canvas.order && self.painted.keys().any(|id| layer.id == ui.id().with(("node_layer", *id))))
        };
        if ui.ctx().layer_id_at(pointer).is_some_and(|layer| layer.order >= canvas.order && !canvas_family(layer)) {
            return false;
        }
        // UI-owned area metadata, inspected only for a fresh candidate. Fail closed without
        // consuming the key if an embedding host exceeds this bounded inspection allowance.
        let layers = ui.ctx().memory(|m| m.areas().visible_layer_ids());
        if layers.len() > MAX_QUICK_ADD_AREAS {
            return false;
        }
        !layers.into_iter().any(|layer| {
            if layer.order < canvas.order || canvas_family(layer) {
                return false;
            }
            egui::AreaState::load(ui.ctx(), layer.id).is_some_and(|area| {
                let rect = area.rect();
                let rect = ui.ctx().layer_transform_to_global(layer).map_or(rect, |transform| transform * rect);
                !rect.is_finite() || rect.contains(pointer)
            })
        })
    }

    /// What every plan §2.1 canvas letter yields to, so that quick-add (#267) and the prompt
    /// overlays' `H` (#321) cannot drift apart. Each term is a reader of state only: nothing here
    /// consumes an event, so a refused letter stays unconsumed for whoever does own it.
    ///
    /// - A gesture in flight (a card drag, a marquee, a wire, an input reorder, a comment drag, or
    ///   a comment gesture just cancelled) owns the canvas for the rest of the gesture.
    /// - `minimap_held`: held minimap input owns canvas keys through release (#361). Quick-add
    ///   passes this frame's frozen admission; `H` runs earlier in the frame, where only a *held*
    ///   minimap gesture exists yet and a fresh press has not been admitted or refused.
    /// - #472: a model swap in flight owns every canvas key. Main's inline quick-add ran inside the
    ///   canvas-keys guard that starts with `!model_blocks`, so this carries it forward (as #460's
    ///   drop guard was).
    /// - #473: the bulk parameter panel owns the pointer and canvas keys while it holds input.
    /// - #460: the pending native-drop control owns the pointer and every canvas key while it is
    ///   held. Main's inline quick-add sat behind that guard (#335).
    /// - An open popup owns canvas keys, as Escape here already assumes: the letter belongs to
    ///   whatever is open, not to the board behind it.
    /// - The "Turn into" menu takes letters for kinds, exactly as a text field does.
    fn canvas_letter_keys_blocked(&self, ui: &egui::Ui, minimap_held: bool, model_blocks: bool, bulk_blocks: bool) -> bool {
        self.drag.is_some()
            || self.marquee.is_some()
            || self.wiring.is_some()
            || self.reordering.is_some()
            || self.comments.drag.is_some()
            || self.comments.gesture_cancelled
            || minimap_held
            || model_blocks
            || bulk_blocks
            || self.navigation.pointer_owned
            || self.navigation.drag.is_some()
            || self.drop_pointer_owned()
            || egui::Popup::is_any_open(ui.ctx())
            || self.text_focused
            || canvas::text_input_focused(ui.ctx())
            || self.palette.is_some()
            || self.turn_into.is_some()
    }

    /// A kind's `quick_add` key creates it under the pointer: a fresh press (not a repeat) with
    /// exactly the bound modifiers, nothing else holding input, and the canvas holding the
    /// pointer. Runs last in the frame so floating Windows have laid out (#267); it keeps every
    /// gate main's inline version had, including the frozen minimap admission from #361 and the
    /// model-swap (#472) and bulk-panel (#473) admissions.
    fn quick_add(&mut self, ui: &egui::Ui, viewport: Rect, minimap_owned: bool, model_blocks: bool, bulk_blocks: bool) {
        // One admission for every canvas letter; pointer ownership is checked after admission.
        if self.canvas_letter_keys_blocked(ui, minimap_owned, model_blocks, bulk_blocks) {
            return;
        }
        // Borrow binding metadata; no owned list or kind string until admission succeeds.
        let candidate = ui.input(|i| {
            i.events.iter().find_map(|event| {
                let egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } = event else { return None };
                self.session.kinds().descriptors().find_map(|d| {
                    let (m, k) = quick_add_key(d.quick_add.as_deref()?)?;
                    (k == *key && m == *modifiers).then_some((m, k, d.id.as_str()))
                })
            })
        });
        let Some((modifiers, key, kind)) = candidate else {
            // `F` makes a field (#66), under the same gates as a kind's key.
            let field = ui.input(|i| {
                i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::F, pressed: true, repeat: false, modifiers, .. } if modifiers.is_none()))
            });
            if field && self.fields.drag.is_none() && self.fields.rename.is_none() && self.canvas_owns_quick_add_pointer(ui) {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F));
                self.quick_field(ui.input(|i| i.pointer.hover_pos()));
                ui.ctx().request_repaint();
            }
            return;
        };
        if !self.canvas_owns_quick_add_pointer(ui) {
            return;
        }
        let kind = kind.to_owned();
        ui.input_mut(|i| i.events.retain(|event| !matches!(event, egui::Event::Key { key: k, modifiers: m, .. } if *k == key && *m == modifiers)));
        let at = ui.input(|i| i.pointer.hover_pos()).filter(|p| viewport.contains(*p)).unwrap_or(viewport.center());
        self.create_node(&kind, self.view.to_world(viewport.min, at));
        ui.ctx().request_repaint();
    }

    /// Top-right button that opens the API keys panel.
    fn keys_button(&mut self, ui: &mut egui::Ui, viewport: Rect) {
        let t = self.theme.tokens();
        let frame = egui::Frame::NONE
            .fill(t.panel_fill)
            .stroke(egui::Stroke::new(1.0, t.panel_stroke))
            .corner_radius(t.panel_radius)
            .inner_margin(egui::Margin::symmetric(6, 4));
        // Left of the search and closed tags controls, which own the top-right corner; overlapping
        // them, a click here also opened search or the tag filter.
        let right = if self.tag_filter.open { self.search_rect().min.x } else { self.tag_rect().min.x.min(self.search_rect().min.x) };
        egui::Area::new(ui.id().with("keys_button"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, Vec2::new(right - viewport.max.x - t.navigation_gap, t.navigation_margin))
            .constrain_to(viewport)
            .show(ui.ctx(), |ui| {
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        self.sheets_button(ui, t);
                        self.techniques_button(ui, t);
                        self.elements_button(ui, t);
                        if let Some(node) = self.selected_technique()
                            && ui
                                .add(egui::Button::new(egui::RichText::new("Open as app").color(t.text_muted)).frame(false))
                                .on_hover_text("Fill the selected technique's inputs and run it from a simple form")
                                .clicked()
                        {
                            self.open_app_mode(node);
                        }
                        if let Some(label) = self.queue_label()
                            && ui
                                .add(egui::Button::new(egui::RichText::new(label).color(t.text_muted)).frame(false))
                                .on_hover_text("Every job: status, provider, cost, cancel and retry")
                                .clicked()
                        {
                            self.queue_open = !self.queue_open;
                        }
                        self.share_button(ui, t);
                        if ui
                            .add(egui::Button::new(egui::RichText::new("API keys").color(t.text_muted)).frame(false))
                            .on_hover_text("Add, check and remove provider keys")
                            .clicked()
                        {
                            self.keys.open = !self.keys.open;
                        }
                        if self.session.enabled("plugins.list", &json!({})).is_ok()
                            && ui
                                .add(egui::Button::new(egui::RichText::new("Plugins").color(t.text_muted)).frame(false))
                                .on_hover_text("Install, enable and grant plugin packages")
                                .clicked()
                        {
                            self.plugin_panel.open = !self.plugin_panel.open;
                        }
                    });
                });
            });
    }

    /// Keeps the remembered account names in settings in step with the keychain.
    /// Records keys saved or deleted, from the keychain's answers or the browser's at once (from
    /// any view's panel); adapters read their keys again.
    fn save_accounts(&mut self, changes: Vec<crate::keys::AccountChange>) {
        if changes.is_empty() {
            return;
        }
        if let Some(runs) = &mut self.runs
            && let Err(error) = runs.forget_adapters(&mut self.session)
        {
            self.notice = Some(error.message);
        }
        let apply = |keys: &mut nodecraft_engine::settings::KeySettings| {
            for change in &changes {
                match change {
                    crate::keys::AccountChange::Remember(account) => keys.remember(account),
                    crate::keys::AccountChange::Forget(account) => keys.forget(account),
                }
            }
        };
        apply(&mut self.key_accounts);
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store {
            store.update(|settings| apply(&mut settings.keys));
        }
    }
    /// Delete the selection (nodes, noodles and the selected field) in one undo step, skipping
    /// ids an undo already removed. A deleted field's cards stay.
    fn dispatch_delete(&mut self) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let nodes: Vec<NodeId> = self.selection.nodes().iter().copied().filter(|id| canvas.nodes().contains_key(id)).collect();
        let edges: Vec<EdgeId> = self.selection.edges().iter().copied().filter(|id| canvas.edges().contains_key(id)).collect();
        let fields: Vec<FieldId> = self.fields.selected.into_iter().filter(|id| canvas.fields().contains_key(id)).collect();
        if nodes.is_empty() && edges.is_empty() && fields.is_empty() {
            return;
        }
        match self.session.dispatch("canvas.delete", &json!({"canvas": self.canvas, "nodes": nodes, "edges": edges, "fields": fields})) {
            Ok(_) => {
                self.notice = None;
                self.selection.clear();
                self.fields.selected = None;
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// The selection as a bounded, history-free clipboard payload.
    fn selection_payload(&self) -> Result<Option<clipboard::Payload>, Error> {
        clipboard::checked_payload(&self.session, self.canvas, &self.selection)
    }
    fn paste_target(&self, ui: &egui::Ui, t: &Tokens) -> Option<Pos2> {
        let usable = self.navigation.usable(self.viewport, t);
        if !canvas::valid_rect(usable) {
            return None;
        }
        let overlay = crate::navigation::layout(self.viewport, self.navigation.visible, t).overlay;
        let screen = ui.input(|i| i.pointer.hover_pos()).filter(|p| self.viewport.contains(*p) && !overlay.contains(*p)).unwrap_or_else(|| usable.center());
        let world = self.view.to_world(self.viewport.min, screen);
        world.is_finite().then_some(world)
    }
    fn duplicate_payload(&mut self, payload: clipboard::Payload, t: &Tokens) {
        if let Some(centre) = clipboard::group_centre(&payload.nodes, |n| payload_rect(&self.session, n, t)) {
            self.paste(payload, centre + Vec2::splat(t.duplicate_offset));
        }
    }
    fn cut_payload(&mut self, ctx: &egui::Context, payload: clipboard::Payload, text: String) {
        let nodes: Vec<_> = payload.nodes.iter().map(|node| node.id).collect();
        let Some(centre) = clipboard::group_centre(&payload.nodes, |n| payload_rect(&self.session, n, self.theme.tokens())) else {
            return;
        };
        let Some((pasted, edges)) = clipboard::place(
            self.session.document(),
            self.canvas,
            payload,
            centre,
            |n| payload_rect(&self.session, n, self.theme.tokens()),
            self.session.revision(),
        ) else {
            self.notice = Some("Couldn't allocate ids for the cut roundtrip; try again.".into());
            return;
        };
        match self.session.dispatch_clipboard_cut(&json!({"canvas":self.canvas,"nodes":nodes}), &json!({"canvas":self.canvas,"nodes":pasted,"edges":edges})) {
            Ok(_) => {
                ctx.copy_text(text);
                self.selection.clear();
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
    /// Paste copied nodes centred on `centre` (one undo step) and select them.
    fn paste(&mut self, payload: clipboard::Payload, centre: Pos2) {
        let t = self.theme.tokens();
        let placed = clipboard::place(self.session.document(), self.canvas, payload, centre, |n| payload_rect(&self.session, n, t), self.session.revision());
        let Some((nodes, edges)) = placed else {
            self.notice = Some("Couldn't allocate ids for the pasted nodes; try again.".into());
            return;
        };
        let pasted: std::collections::BTreeSet<NodeId> = nodes.iter().map(|n| n.id).collect();
        match self.session.dispatch("canvas.paste", &json!({"canvas": self.canvas, "nodes": nodes, "edges": edges})) {
            Ok(_) => {
                self.notice = None;
                self.selection.replace(pasted);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// The `history.*` command for a strip intent on node `id`. Extracted results land to the
    /// right of their node, under fresh ids.
    fn history_command(&mut self, id: NodeId, intent: HistoryIntent, t: &Tokens) -> Option<(&'static str, serde_json::Value)> {
        let at = json!({"canvas": self.canvas, "id": id});
        Some(match intent {
            HistoryIntent::Previous => ("history.previous", at),
            HistoryIntent::Next => ("history.next", at),
            HistoryIntent::Clear => ("history.clear", at),
            HistoryIntent::Select(generation) => ("history.select", json!({"canvas": self.canvas, "id": id, "generation": generation})),
            HistoryIntent::Delete(generation) => ("history.delete", json!({"canvas": self.canvas, "id": id, "generation": generation})),
            HistoryIntent::Extract { generation, port, item } => {
                let document = self.session.document();
                let canvas = document.canvases().get(&self.canvas)?;
                let source = canvas.nodes().get(&id)?;
                let Some(node) = ids::node_id(document, ("extract", id, generation, self.session.revision())) else {
                    self.notice = Some("Couldn't find a free id for the extracted result".into());
                    return None;
                };
                // Beside the node, stepping down past earlier extracts rather than on top of them.
                let x = source.pos.x + t.card_width + t.card_padding * 4.0;
                let step = t.card_min_height + t.card_padding * 2.0;
                let taken = |y: f32| canvas.nodes().values().any(|n| (n.pos.x - x).abs() < t.card_width / 2.0 && (n.pos.y - y).abs() < step / 2.0);
                let y = (0..64u8).map(|row| source.pos.y + f32::from(row) * step).find(|y| !taken(*y)).unwrap_or(source.pos.y);
                let mut params = json!({"canvas": self.canvas, "id": id, "generation": generation, "node": node, "pos": {"x": x, "y": y}});
                if let Some(port) = port {
                    params["port"] = json!(port);
                }
                if let Some(item) = item {
                    params["item"] = json!(item);
                }
                ("history.extract", params)
            }
            HistoryIntent::Export { generation, port, item, name } => {
                self.start_export(self.canvas, id, generation, port, item, &name);
                return None;
            }
        })
    }

    /// Paste plain text as a Text node centred on `centre`.
    fn paste_text(&mut self, text: &str, centre: Pos2) {
        if text.trim().is_empty() {
            return;
        }
        let t = self.theme.tokens();
        let mut values = BTreeMap::new();
        values.insert("text".to_owned(), Value::Text(text.to_owned()));
        let node = ids::node_id(self.session.document(), ("paste-text", self.session.revision(), text.len()))
            .ok_or_else(|| "Couldn't allocate a node id; try again.".to_owned())
            .and_then(|id| {
                self.session.kinds().get("source.text").and_then(|kind| kind.instantiate_instance(id, BTreeMap::new(), &values)).map_err(|error| error.message)
            });
        match node {
            Ok(mut node) => {
                let rect = payload_rect(&self.session, &node, t);
                let shift = centre - rect.center();
                node.pos = nodecraft_graph::Position { x: node.pos.x + shift.x, y: node.pos.y + shift.y };
                let id = node.id;
                match self.session.dispatch("canvas.paste", &json!({"canvas": self.canvas, "nodes": [node]})) {
                    Ok(_) => {
                        self.notice = None;
                        self.selection.only(id);
                    }
                    Err(error) => self.notice = Some(error.message),
                }
            }
            Err(message) => self.notice = Some(message),
        }
    }

    /// Create a node of `kind` centred on `world`, select it, and report failures as a notice.
    fn create_node(&mut self, kind: &str, world: Pos2) {
        let t = self.theme.tokens();
        let pos = nodecraft_graph::Position { x: world.x - t.card_width * 0.5, y: world.y - t.card_min_height * 0.5 };
        let seed = (kind, self.session.revision(), pos.x.to_bits(), pos.y.to_bits());
        let Some(id) = ids::node_id(self.session.document(), seed) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        match self.session.dispatch("node.create", &json!({"canvas": self.canvas, "id": id, "kind": kind, "pos": pos})) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Creates `kind` where the "Turn into" menu opened, fed by its noodle, as one undo step.
    fn create_connected(&mut self, kind: &str, menu: &TurnInto) {
        let t = self.theme.tokens();
        // The new card's input side sits where the noodle was dropped.
        let pos = nodecraft_graph::Position { x: menu.world.x, y: menu.world.y - t.card_min_height * 0.5 };
        let document = self.session.document();
        let seed = ("turn-into", kind, self.session.revision(), pos.x.to_bits(), pos.y.to_bits());
        let (Some(id), Some(edge)) = (ids::node_id(document, seed), ids::edge_id(document, seed)) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        let params = json!({"canvas": self.canvas, "id": id, "kind": kind, "pos": pos, "from": menu.from, "edge": edge});
        match self.session.dispatch("node.create_connected", &params) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// What the palette offers: every node kind, then every command that runs without
    /// parameters right now, plus the active canvas staleness query.
    fn palette_entries(&self) -> Vec<palette::Entry> {
        let mut kinds: Vec<palette::Entry> = self
            .session
            .kinds()
            .descriptors()
            .map(|d| palette::Entry { target: palette::Target::Kind(d.id.clone()), label: d.label.clone(), shortcut: d.quick_add.clone() })
            .collect();
        kinds.sort_by(|a, b| a.label.cmp(&b.label));
        let commands = self
            .session
            .registry()
            .specs()
            .filter(|spec| {
                // They pick their file when chosen.
                if matches!(spec.id, "file.open" | "file.save" | "file.save_as") {
                    return self.session.file_status().available;
                }
                self.session.enabled(spec.id, &self.palette_command_params(spec.id)).is_ok()
            })
            .map(|spec| palette::Entry { target: palette::Target::Command(spec.id), label: spec.label.to_owned(), shortcut: spec.shortcut.map(str::to_owned) });
        kinds.into_iter().chain(commands).collect()
    }

    /// Paint the noodle being dragged and, once the pointer is released, choose the command it
    /// becomes (and a notice when the drop target can't accept it).
    /// One frame of a noodle being dragged: the command its drop runs, a notice explaining a
    /// refused drop, or the "Turn into" menu for a new noodle dropped on empty canvas.
    fn wire(
        &self,
        ui: &egui::Ui,
        canvas: &Canvas,
        wiring: &Wiring,
        pointer: Pos2,
        t: &Tokens,
    ) -> (Option<(&'static str, serde_json::Value)>, Option<String>, Option<TurnInto>) {
        let kinds = self.session.kinds();
        let origin = self.viewport.min;
        let Some(start) = canvas
            .nodes()
            .get(&wiring.from.node)
            .and_then(|source| noodle::port_anchor(source, card_geometry(&self.painted, kinds, source, t), Side::Output, &wiring.from.port))
        else {
            return (None, None, None);
        };
        // Nodes that already feed the source: connecting into any of them would close a loop.
        let feeding = selection::upstream(canvas, &[wiring.from.node].into_iter().collect());
        // Inputs this output already feeds (other than the noodle being moved), built once so
        // each candidate input is a set lookup rather than a scan of every edge.
        let fed: std::collections::BTreeSet<(NodeId, &str)> =
            canvas.edges().values().filter(|e| e.from == wiring.from && Some(e.id) != wiring.moving).map(|e| (e.to.node, e.to.port.as_str())).collect();
        let accepts = |node: &Node, input: &nodecraft_graph::Port| -> Result<(), String> {
            if node.id == wiring.from.node {
                Err("A node can't connect to itself.".into())
            } else if !noodle::compatible(wiring.port_type, input.port_type) {
                Err(if wiring.port_type.modality == input.port_type.modality {
                    "A list output needs a list input.".into()
                } else {
                    let mut from = noodle::modality_phrase(wiring.port_type.modality).chars();
                    let from: String = from.next().map(|c| c.to_uppercase().chain(from).collect()).unwrap_or_default();
                    format!("{from} output can't connect to {} input.", noodle::modality_phrase(input.port_type.modality))
                })
            } else if feeding.contains(&node.id) {
                Err("That connection would create a loop.".into())
            } else if fed.contains(&(node.id, input.name.as_str())) {
                Err("These ports are already connected.".into())
            } else {
                Ok(())
            }
        };
        let world = self.view.to_world(origin, pointer);
        let reach = card::PORT_HIT / self.view.zoom + t.port_radius;
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("wiring"))).with_clip_rect(self.viewport);
        // The nearest accepting input within reach wins; failing that, the nearest input explains why.
        let mut best: Option<(bool, f32, &Node, &nodecraft_graph::Port, Pos2)> = None;
        for (node, rect) in drop_targets(canvas, &self.painted) {
            for (index, input) in node.inputs.iter().enumerate() {
                let at = noodle::anchor(rect, Side::Input, index, node.inputs.len());
                let ok = accepts(node, input).is_ok();
                if ok {
                    let ring = t.port_radius * self.view.zoom + t.drop_ring_gap;
                    painter.circle_stroke(self.view.to_screen(origin, at), ring, egui::Stroke::new(t.drop_ring_width, t.wire(input.port_type.modality)));
                }
                let distance = at.distance(world);
                if distance <= reach && best.is_none_or(|(best_ok, d, ..)| (ok && !best_ok) || (ok == best_ok && distance < d)) {
                    best = Some((ok, distance, node, input, at));
                }
            }
        }
        let target = best.map(|(_, _, node, input, at)| (node, input, at));
        let end = target.filter(|(node, input, _)| accepts(node, input).is_ok()).map_or(world, |(_, _, at)| at);
        let points = noodle::bezier(start, end).map(|p| self.view.to_screen(origin, p));
        let stroke = egui::Stroke::new(t.wire_width, t.wire(wiring.port_type.modality));
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(points, false, egui::Color32::TRANSPARENT, stroke));
        if ui.input(|i| i.pointer.primary_down()) {
            return (None, None, None);
        }
        let Some((node, input, _)) = target else {
            // Dropping a picked-up noodle on empty canvas disconnects it; a new one asks what
            // to turn into.
            if let Some(id) = wiring.moving {
                return (Some(("edge.disconnect", json!({"canvas": self.canvas, "id": id}))), None, None);
            }
            // Only empty canvas: not on a card, and not under an overlay (minimap, search).
            let on_card = drop_targets(canvas, &self.painted).any(|(_, rect)| rect.contains(world));
            let on_canvas = self.viewport.contains(pointer) && ui.ctx().layer_id_at(pointer).is_none_or(|layer| layer == ui.layer_id());
            if on_card || !on_canvas {
                return (None, None, None);
            }
            return (None, None, Some(TurnInto::new(self.session.kinds(), wiring.from.clone(), wiring.port_type, world, pointer)));
        };
        if let Err(message) = accepts(node, input) {
            return (None, Some(message), None);
        }
        let to = Endpoint { node: node.id, port: input.name.clone() };
        let occupants: Vec<_> = noodle::into_port(canvas, &to).into_iter().filter(|e| Some(e.id) != wiring.moving).collect();
        if wiring.moving.is_some_and(|id| canvas.edges().get(&id).is_some_and(|e| e.to == to)) {
            return (None, None, None);
        }
        let mut replace: Vec<EdgeId> = wiring.moving.into_iter().collect();
        let order = if input.port_type.many {
            match noodle::next_order(canvas, &to, wiring.moving) {
                Some(order) => order,
                None => return (None, Some("This input can't take more connections.".into()), None),
            }
        } else {
            // A single input swaps its current noodle for the new one, in the same undo step.
            replace.extend(occupants.first().map(|e| e.id));
            0
        };
        let Some(id) = wiring.moving.or_else(|| noodle::new_edge_id(self.session.document(), &wiring.from, &to, self.session.revision())) else {
            return (None, Some("Couldn't allocate a connection id; try again.".into()), None);
        };
        let edge = json!({"id": id, "from": wiring.from, "to": to, "order": order});
        if replace.is_empty() {
            (Some(("edge.connect", json!({"canvas": self.canvas, "edge": edge}))), None, None)
        } else {
            (Some(("edge.rewire", json!({"canvas": self.canvas, "edge": edge, "replace": replace}))), None, None)
        }
    }
}
/// A transformed card painter needs its clip in the same world coordinates as its shapes.
fn tag_world_clip(view: &CanvasView, viewport: Rect, parent_clip: Rect) -> Rect {
    let screen = viewport.intersect(parent_clip);
    Rect::from_min_max(view.to_world(viewport.min, screen.min), view.to_world(viewport.min, screen.max))
}
fn consume_shortcut(input: &mut egui::InputState, shortcut: &str) -> bool {
    consume_shortcut_with_policy(input, shortcut, false)
}
/// Was a bare canvas letter (#321's `H`) pressed this frame, fresh rather than repeating?
///
/// A plan §2.1 letter is a letter, and Shift is how a person types a capital, so Shift is allowed
/// and any other modifier is not; unlike `InputState::consume_key`, which matches extra Shift and
/// Alt and includes auto-repeat, this refuses a repeat, because `H` toggles and strobing the board
/// while a key sits under a finger is not a clean board. Consumes every press it accepts, and
/// consumes nothing when the letter never arrived, so a refused owner keeps the event.
fn consume_canvas_letter(input: &mut egui::InputState, letter: egui::Key) -> bool {
    let mut pressed = false;
    input.events.retain(|event| {
        let matched = matches!(event, egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. }
            if *key == letter && !modifiers.command && !modifiers.ctrl && !modifiers.alt);
        pressed |= matched;
        !matched
    });
    pressed
}
fn consume_navigation_shortcut(input: &mut egui::InputState, shortcut: &str) -> bool {
    consume_shortcut_with_policy(input, shortcut, true)
}
fn consume_shortcut_with_policy(input: &mut egui::InputState, shortcut: &str, navigation: bool) -> bool {
    let mut modifiers = egui::Modifiers::NONE;
    let mut key = None;
    for part in shortcut.split('+') {
        match part {
            "Command" => modifiers |= egui::Modifiers::COMMAND,
            "Ctrl" => modifiers |= egui::Modifiers::CTRL,
            "Shift" => modifiers |= egui::Modifiers::SHIFT,
            "Alt" => modifiers |= egui::Modifiers::ALT,
            "Z" => key = Some(egui::Key::Z),
            "Y" => key = Some(egui::Key::Y),
            "1" => key = Some(egui::Key::Num1),
            "2" => key = Some(egui::Key::Num2),
            "M" => key = Some(egui::Key::M),
            "F" => key = Some(egui::Key::F),
            "S" => key = Some(egui::Key::S),
            "O" => key = Some(egui::Key::O),
            "C" => key = Some(egui::Key::C),
            "Enter" => key = Some(egui::Key::Enter),
            "Escape" => key = Some(egui::Key::Escape),
            _ => return false,
        }
    }
    key.is_some_and(|key| {
        if !navigation {
            return input.consume_key(modifiers, key);
        }
        let digit = matches!(key, egui::Key::Num1 | egui::Key::Num2);
        let mut consumed = false;
        input.events.retain(|event| {
            let matched = matches!(event, egui::Event::Key { key: logical, physical_key, pressed: true, modifiers: actual, .. }
                if actual.matches_exact(modifiers) && if digit { physical_key.unwrap_or(*logical) == key } else { *logical == key });
            consumed |= matched;
            !matched
        });
        consumed
    })
}

#[cfg(not(target_arch = "wasm32"))]
impl NodeCraftApp {
    /// Persist before entering a potentially stuck driver. The injected drain also keeps
    /// lifecycle tests independent from captures owned by other concurrent app fixtures.
    fn finish_native_exit_with(&mut self, drain: impl FnOnce(std::time::Duration) -> nodecraft_engine::action_exec::CaptureShutdown) {
        self.close_storage();
        // A read-only store (newer schema, unwritable folder) was reported at startup.
        if let Some(store) = &mut self.store
            && !store.is_read_only()
            && let Err(e) = store.flush()
        {
            log::warn!("settings not saved on exit: {e}");
        }
        let deadline = std::time::Instant::now() + nodecraft_engine::action_exec::CAPTURE_CLOSE_WAIT;
        self.live.cancel();
        if let Some(runs) = &mut self.runs {
            runs.shut_down();
        }
        let captures = drain(deadline.saturating_duration_since(std::time::Instant::now()));
        if captures.remaining > 0 {
            log::warn!("3D capture close deadline expired with {} captures still running", captures.remaining);
        }
        if captures.panicked > 0 {
            log::warn!("{} 3D capture workers panicked", captures.panicked);
        }
        self.live.stop(deadline);
    }
}

impl eframe::App for NodeCraftApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let title = self.window_title();
            if title != self.title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.title = title;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(results) = &self.key_results {
            self.keys.tick(ctx.input(|i| i.time));
            let (changes, others) = self.keys.receive(results.drain());
            // Nothing else asks the keychain yet; answers to other requests are logged, not lost.
            for outcome in others {
                log::info!("keychain answered request {} for {}", outcome.id, outcome.account);
            }
            self.save_accounts(changes);
            if self.keys.is_waiting() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &mut self.store {
            let tick = store.tick(std::time::Instant::now());
            if let nodecraft_engine::settings::SaveStatus::Failed(e) = &tick.status {
                log::warn!("settings not saved, will retry: {e}");
            }
            for moved in store.take_moved_aside() {
                log::warn!("settings file was damaged and moved to {}", moved.display());
            }
            if let Some(deadline) = tick.next_deadline {
                ctx.request_repaint_after(deadline.saturating_duration_since(std::time::Instant::now()));
            }
        }
        self.tick_runs(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_option_fetch(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_comfy_nodes(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_updates(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_plugins(ctx);
        self.tick_sharing(ctx);
        // Last, so it sees any `CancelClose` from an unsaved-changes check (none in M0).
        craft_quit::on_close_request(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
    fn on_exit(&mut self) {
        self.audios.stop();
        #[cfg(not(target_arch = "wasm32"))]
        self.finish_native_exit_with(nodecraft_engine::action_exec::shutdown_captures);
        // Last: storage is closed and captures drained before the updated copy opens (#540).
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(updates) = &self.updates {
            let project = match self.session.file_status().location {
                Some(nodecraft_format::StorageLocation::Path(path)) => Some(path),
                _ => None,
            };
            updates.relaunch(project.as_deref());
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.theme.tokens().canvas_bg.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod action_tests;
#[cfg(test)]
mod app_mode_tests;
#[cfg(test)]
mod audio672_tests;
#[cfg(test)]
mod audio_tests;
#[cfg(test)]
mod comfy_workflow9_tests;
#[cfg(test)]
mod deck_editor_tests;
#[cfg(test)]
mod deck_presenter_tests;
#[cfg(test)]
mod element_library_view_tests;
#[cfg(test)]
mod elements_tests;
#[cfg(test)]
mod export_tests;
#[cfg(test)]
mod fields_tests;
#[cfg(test)]
mod gallery_tests;
#[cfg(test)]
mod generate_tests;
#[cfg(test)]
mod generation_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod image_tools_tests;
#[cfg(test)]
mod layer_editor_tests;
#[cfg(test)]
mod library_view_tests;
#[cfg(test)]
mod mask_editor_tests;
#[cfg(test)]
mod mentions_tests;
#[cfg(test)]
mod mesh_tests;
#[cfg(test)]
mod order_tests;
#[cfg(test)]
mod perf;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod plugin_tests;
#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod prompt_overlay_tests;
#[cfg(test)]
mod reader_tests;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod runs_tests;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod sheet_tests;
#[cfg(test)]
mod split_tests;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod stream_tests;
#[cfg(test)]
mod styles_tests;
#[cfg(test)]
mod technique_menu_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_card_tests;
#[cfg(test)]
mod timeline_editor_tests;
#[cfg(test)]
mod video559_tests;
#[cfg(test)]
mod video_tests;

#[cfg(test)]
mod marquee_tests;
#[cfg(test)]
mod turn_into_tests;

#[cfg(test)]
mod held_minimap_tests;
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod held_restart_tests;
#[cfg(test)]
mod stale_tests;
