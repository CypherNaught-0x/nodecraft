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
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::json;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

pub(crate) const MAX_SCHEMA_CACHE_BYTES: usize = 16 * 1024 * 1024;
struct CachedSchema {
    descriptor: Arc<nodecraft_nodes::NodeDescriptor>,
    visible: BTreeSet<String>,
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
        let names = entry.visible.iter().map(|name| name.len().saturating_add(96)).fold(0usize, usize::saturating_add);
        let owned = if entry.borrowed_identity.is_none() { entry.descriptor_bytes } else { 0 };
        256usize.saturating_add(names).saturating_add(owned)
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
    if cache.get(&node.id).is_some_and(|entry| entry.revision == session.revision()) {
        if let Some(entry) = cache.entries.get_mut(&node.id) {
            entry.last_used = clock;
        }
        return cache.get(&node.id);
    }
    let descriptor = session.kinds().get(&node.kind).ok()?.resolve_instance(node).ok()?;
    if node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
        return None;
    }
    let nodecraft_nodes::ResolvedParams { visible, .. } = descriptor.params.resolve(&node.params, false).ok()?;
    let (descriptor, borrowed_identity, bytes) = match descriptor {
        Cow::Borrowed(descriptor) => {
            // Safe pointer identity only: no raw-pointer dereference. Distinct borrowed variants
            // of one dynamic kind remain distinct; the registry outlives this view cache.
            let identity = descriptor as *const nodecraft_nodes::NodeDescriptor as usize;
            if let Some((shared, bytes, _)) = cache.shared.get(&identity) {
                (Arc::clone(shared), Some(identity), *bytes)
            } else {
                let bytes = descriptor_bytes(descriptor)?;
                (Arc::new(descriptor.clone()), Some(identity), bytes)
            }
        }
        Cow::Owned(descriptor) => {
            let bytes = descriptor_bytes(&descriptor)?;
            (Arc::new(descriptor), None, bytes)
        }
    };
    cache.insert(node.id, CachedSchema { descriptor, visible, descriptor_bytes: bytes, borrowed_identity, revision: session.revision(), last_used: clock });
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
fn payload_rect(kinds: &nodecraft_nodes::Registry, node: &nodecraft_graph::Node, t: &theme::Tokens) -> Rect {
    card::world_rect(node, kinds.get(&node.kind).ok().and_then(|kind| kind.resolve_instance(node).ok()).as_deref(), t)
}

/// World offset of a duplicate from its original.
const DUPLICATE_OFFSET: f32 = 40.0;

/// Screen points within which a click selects a noodle.
const NOODLE_HIT: f32 = 6.0;

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

/// A card's world rect: as painted this frame, or (for cards not drawn, e.g. off screen) from the
/// kind's static descriptor.
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
fn box_geometry(painted: &BTreeMap<NodeId, Rect>, kinds: &nodecraft_nodes::Registry, node: &nodecraft_graph::Node, world: Rect, t: &theme::Tokens) -> Rect {
    if painted.contains_key(&node.id) {
        return card_geometry(painted, kinds, node, t);
    }
    let largest = Rect::from_min_size(Pos2::new(node.pos.x, node.pos.y), Vec2::new(t.card_width, t.card_max_height));
    if !largest.intersects(world) {
        return largest;
    }
    let resolved = kinds.get(&node.kind).ok().and_then(|kind| kind.resolve_instance(node).ok());
    card::world_rect(node, resolved.as_deref(), t)
}

mod comments;
mod drop;
mod layout;
#[cfg(not(target_arch = "wasm32"))]
mod live;
mod metrics;
mod navigation;
mod order;
#[cfg(not(target_arch = "wasm32"))]
mod recovery;
mod search;
mod split;
#[cfg(not(target_arch = "wasm32"))]
pub mod storage;
mod tags;

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
    pub viewport: Rect,
    pub notice: Option<String>,
    editors: BTreeMap<NodeId, EditorState>,
    drag: Option<Drag>,
    marquee: Option<Marquee>,
    /// World rects of the cards painted this frame. Geometry for selection (and noodles) comes
    /// from here, not the evictable schema cache, so it matches what is on screen.
    painted: BTreeMap<NodeId, Rect>,
    wiring: Option<Wiring>,
    /// The input order badge being dragged: its list input and edge.
    reordering: Option<(crate::order::Input, EdgeId)>,
    palette: Option<Palette>,
    /// The startup question about an earlier session's unsaved work (#408).
    #[cfg(not(target_arch = "wasm32"))]
    recovery: Option<recovery::RecoveryOffer>,
    /// Live recompute of action nodes (#88).
    #[cfg(not(target_arch = "wasm32"))]
    live: live::Live,
    /// Recovered work's kept journal, removed once that work is saved (#408).
    #[cfg(not(target_arch = "wasm32"))]
    recovered: Option<recovery::RecoveryOffer>,
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
    /// When the last timed frame started, for the frame interval (#151).
    last_frame: Option<u64>,
    frame_workload: Option<nodecraft_engine::perf::Workload>,
    /// The rows × columns last chosen for "Split into grid".
    split_size: (u32, u32),
    /// The split being picked: seen in this frame's open menu, shown on the card next frame.
    split_seen: Option<(NodeId, u32, u32)>,
    split_shown: Option<(NodeId, u32, u32)>,
    /// The "Turn into" menu after a new noodle was dropped on empty canvas.
    turn_into: Option<TurnInto>,
    /// egui drops focus on a bare Escape before the frame runs, so remember last frame's.
    text_focused: bool,
    /// Exact previous text owner survives Escape and disabled-toolbar interaction.
    last_text_focus: Option<egui::Id>,
    schemas: SchemaCache,
    cache_clock: u64,
    next_gesture: u64,
    centred: bool,
    /// The API keys panel, and the accounts it lists (from settings).
    pub keys: crate::keys::KeysPanel,
    pub key_accounts: nodecraft_engine::settings::KeySettings,
    /// Answers from the keychain thread (the app's `keys.store` service).
    #[cfg(not(target_arch = "wasm32"))]
    key_results: Option<nodecraft_engine::secrets::KeyResults>,
}
impl NodeCraftApp {
    /// Cancel one owner at a time: noodle, input order, marquee, then selection.
    fn cancel_canvas_gesture(&mut self) {
        if self.wiring.take().is_some() {
            return;
        }
        if self.reordering.take().is_some() {
            return;
        }
        match &mut self.marquee {
            Some(marquee) if !marquee.cancelled => {
                marquee.cancelled = true;
                self.selection = marquee.base.clone();
            }
            _ => self.selection.clear(),
        }
    }

    pub fn new(ctx: &egui::Context) -> Result<Self, Error> {
        let mut app = Self::headless()?;
        app.apply_settings(ctx, &NodeCraftSettings::default());
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
        // Plugin data packages (#172) live in the settings folder; without one the plugin.*
        // commands are unsupported, and a store that can't open is logged, not fatal.
        let mut plugin_problem = None;
        if let Some(dir) = store.dir().map(|d| d.join("plugins"))
            && let Err(error) = Self::register_plugins(&mut services, dir)
        {
            log::warn!("plugin packages unavailable: {}", error.message);
            plugin_problem = Some(format!("Plugin packages are unavailable: {}", error.message));
        }
        // Storage that can't start (another window holds the journal, or the project won't
        // open) still opens the window, without project files, and says why.
        let mut problem = None;
        let journal = storage.as_ref().map(|setup| setup.recovery.clone());
        let stored = storage.and_then(|setup| match Self::storage_session(setup) {
            Ok(stored) => Some(stored),
            Err(error) => {
                log::error!("project storage unavailable: {}", error.message);
                problem = Some(format!("Project files are unavailable: {}", error.message));
                None
            }
        });
        let mut kept = None;
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
        // Unsaved work from a session that didn't close cleanly: ask before anything else (#408).
        if let Some(journal) = journal {
            app.attach_recovery(journal, kept);
        }
        app.key_results = results;
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
        theme::apply(ctx, self.theme);
    }

    pub fn headless() -> Result<Self, Error> {
        Self::with_services(nodecraft_engine::commands::Services::default())
    }

    /// The `plugins` service over the package store in `dir` (`store/`, `trust.json`). No
    /// package may take an ID the app registers; no host implementation is bindable yet.
    #[cfg(not(target_arch = "wasm32"))]
    fn register_plugins(services: &mut nodecraft_engine::commands::Services, dir: std::path::PathBuf) -> Result<(), Error> {
        use nodecraft_engine::plugin_cmds::{PluginHost, register_plugin_service};
        let mut taken: std::collections::BTreeSet<String> = nodecraft_engine::commands::Registry::builtins()?.specs().map(|s| s.id.to_owned()).collect();
        taken.extend(nodecraft_nodes::Registry::builtins()?.descriptors().map(|d| d.id.clone()));
        let store = nodecraft_plugin_store::Store::open(nodecraft_plugin_store::FolderBackend::new(dir.join("store"))?)?;
        register_plugin_service(services, store, PluginHost { trust: Some(dir.join("trust.json")), taken, bindings: Default::default() })
    }

    /// The headless app with these host services on its session (the keychain, for one).
    pub fn with_services(services: nodecraft_engine::commands::Services) -> Result<Self, Error> {
        let kinds = nodecraft_nodes::Registry::builtins()?;
        let (document, canvas) = Self::starter_document(&kinds)?;
        let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins()?, kinds)?.with_services(services);
        Self::from_session(session, canvas)
    }

    /// The browser build has no project files yet (#99).
    #[cfg(target_arch = "wasm32")]
    fn file_shortcut(&mut self, _save: bool, _save_as: bool) -> Option<String> {
        Some("Saving isn't available in the browser yet.".into())
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
            viewport: Rect::NOTHING,
            notice: None,
            editors: BTreeMap::new(),
            drag: None,
            marquee: None,
            painted: BTreeMap::new(),
            wiring: None,
            reordering: None,
            palette: None,
            #[cfg(not(target_arch = "wasm32"))]
            recovery: None,
            #[cfg(not(target_arch = "wasm32"))]
            live: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            recovered: None,
            #[cfg(not(target_arch = "wasm32"))]
            recovery_journal: None,
            #[cfg(not(target_arch = "wasm32"))]
            recorded_location: None,
            title: String::new(),
            previews: crate::preview::Previews::default(),
            last_frame: None,
            frame_workload: None,
            split_size: (2, 2),
            split_seen: None,
            split_shown: None,
            turn_into: None,
            text_focused: false,
            last_text_focus: None,
            schemas: SchemaCache::default(),
            cache_clock: 0,
            next_gesture: 0,
            centred: false,
            keys: crate::keys::KeysPanel::default(),
            key_accounts: nodecraft_engine::settings::KeySettings::default(),
            #[cfg(not(target_arch = "wasm32"))]
            key_results: None,
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
        let started = self.frame_started();
        self.show_frame(ui);
        self.frame_finished(started);
    }

    fn show_frame(&mut self, ui: &mut egui::Ui) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let t = *self.theme.tokens();
            self.recovery_dialog(ui.ctx(), &t);
            self.track_recovery();
            // Not while the startup question is open: its answer may replace the document.
            if self.recovery.is_none() {
                self.live_recompute(ui.ctx());
            }
        }
        // The grid picked in last frame's menu shows this frame; it goes when the menu closes.
        self.split_shown = self.split_seen.take();
        self.previews.poll(ui.ctx());
        let surrender_focus_on = ui.ctx().options(|o| o.input_options.surrender_focus_on);
        self.cache_clock = self.cache_clock.saturating_add(1);
        let t = self.theme.tokens();
        let viewport = ui.max_rect();
        self.viewport = viewport;
        if !self.centred {
            let home = self.home();
            self.view.reset(viewport, home);
            self.centred = true;
        }
        let comments_blocks = self.comments_overlay(ui);
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
        let tag_blocks = self.tag_overlay(ui);
        let search_blocks = self.search_overlay(ui);
        let selected_before = self.selection.clone();
        let mut minimap_press = None;
        // Save and open (#406) work from a text field too (no field uses them); not while the palette
        // or "Turn into" is open. Without a file yet, saving needs a location: say how to give one.
        if self.palette.is_none() && self.turn_into.is_none() {
            let (save_as, save, open) =
                ui.input_mut(|i| (consume_shortcut(i, "Command+Shift+S"), consume_shortcut(i, "Command+S"), consume_shortcut(i, "Command+O")));
            if save || save_as || open {
                self.notice = self.file_shortcut(save, save_as);
            }
        }
        // Canvas keys yield to text fields and to the open palette (whose query may lose focus)
        // or "Turn into" menu (whose letters pick kinds).
        if !canvas::text_input_focused(ui.ctx()) && self.palette.is_none() && self.turn_into.is_none() {
            let command = ui.input_mut(|input| {
                self.session.registry().specs().filter(|spec| matches!(spec.id, "edit.undo" | "edit.redo")).find_map(|spec| {
                    spec.shortcut
                        .into_iter()
                        .chain(spec.alternate_shortcuts.iter().copied())
                        .any(|shortcut| consume_shortcut(input, shortcut))
                        .then_some(spec.id)
                })
            });
            if let Some(command) = command {
                if let Err(error) = self.session.dispatch(command, &json!({})) {
                    self.notice = Some(error.message);
                } else {
                    self.notice = None;
                    // Invalid drafts remain view state across unrelated undo/redo.
                }
            }
            let (select_all, escape, delete) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                    i.key_pressed(egui::Key::Escape),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                )
            });
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
                || minimap_owned
                || self.comments.drag.is_some()
                || self.comments.gesture_cancelled
                || egui::Popup::is_any_open(ui.ctx())
                || self.keys.open;
            let (copy, cut, pasted, duplicate) = ui.input_mut(|i| {
                // Yield before consuming shortcuts or cloning any pasted payload.
                if busy {
                    return (false, false, None, false);
                }
                let duplicate = i.consume_key(egui::Modifiers::COMMAND, egui::Key::D);
                let (mut copy, mut cut, mut pasted) = (false, false, None);
                for event in &i.events {
                    match event {
                        egui::Event::Copy => copy = true,
                        egui::Event::Cut => cut = true,
                        // Bounded before any copy: oversized text is refused, not cloned.
                        egui::Event::Paste(text) if text.len() > clipboard::MAX_BYTES => pasted = Some(Err(())),
                        egui::Event::Paste(text) => pasted = Some(Ok(text.clone())),
                        _ => {}
                    }
                }
                (copy, cut, pasted, duplicate)
            });
            let pointer_world = ui.input(|i| i.pointer.hover_pos()).filter(|p| viewport.contains(*p)).map(|p| self.view.to_world(viewport.min, p));
            if (copy || cut || duplicate)
                && let Some(payload) = self.selection_payload()
            {
                // A copy that couldn't be pasted back is refused, so cut never loses work.
                if let Err(why) = self.pasteable(&payload) {
                    self.notice = Some(why);
                } else if duplicate {
                    let kinds = self.session.kinds();
                    if let Some(centre) = clipboard::group_centre(&payload.nodes, |n| payload_rect(kinds, n, t)) {
                        self.paste(payload, centre + Vec2::splat(DUPLICATE_OFFSET));
                    }
                } else if let Some(text) = clipboard::to_text(&payload) {
                    ui.ctx().copy_text(text);
                    if cut {
                        // Cut removes what it copied: the selected nodes (their noodles go with them).
                        let nodes = self.selection.nodes().clone();
                        self.dispatch_delete_of(&nodes, &Default::default());
                    }
                }
            }
            match pasted {
                Some(Err(())) => self.notice = Some("The clipboard holds more than 16 MiB; nothing was pasted.".into()),
                Some(Ok(text)) => {
                    let centre = pointer_world.unwrap_or_else(|| self.view.to_world(viewport.min, viewport.center()));
                    match clipboard::parse(&text) {
                        clipboard::Parsed::Nodes(payload) if payload.project != Some(self.session.document().id()) && clipboard::has_media(&payload) => {
                            self.notice = Some(
                                "Can't paste these nodes: their media lives in another project, and copying media between projects isn't supported yet.".into(),
                            );
                        }
                        clipboard::Parsed::Nodes(payload) => self.paste(payload, centre),
                        clipboard::Parsed::Unreadable(why) => self.notice = Some(format!("Can't paste these nodes: {why}.")),
                        clipboard::Parsed::Text => self.paste_text(&text, centre),
                    }
                }
                None => {}
            }
            if select_all && let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
                self.selection.replace(canvas.nodes().keys().copied().collect());
            }
            // Quick-add: a kind's `quick_add` key creates it under the pointer. Only a fresh press
            // (not key repeat) with exactly the bound modifiers, and not mid-gesture or in a menu.
            let busy = self.drag.is_some()
                || self.marquee.is_some()
                || self.wiring.is_some()
                || self.reordering.is_some()
                || self.comments.drag.is_some()
                || self.comments.gesture_cancelled
                || minimap_owned
                || egui::Popup::is_any_open(ui.ctx());
            if !self.text_focused && !busy {
                let bindings: Vec<(egui::Modifiers, egui::Key, String)> =
                    self.session.kinds().descriptors().filter_map(|d| quick_add_key(d.quick_add.as_deref()?).map(|(m, k)| (m, k, d.id.clone()))).collect();
                let pressed = ui.input_mut(|i| {
                    let found = i.events.iter().find_map(|event| match event {
                        egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
                            bindings.iter().find(|(m, k, _)| k == key && m == modifiers).map(|(m, k, kind)| (*m, *k, kind.clone()))
                        }
                        _ => None,
                    });
                    let (modifiers, key, kind) = found?;
                    // Consume every event of that key, repeats included.
                    i.events.retain(|event| !matches!(event, egui::Event::Key { key: k, modifiers: m, .. } if *k == key && *m == modifiers));
                    Some(kind)
                });
                if let Some(kind) = pressed {
                    let at = ui.input(|i| i.pointer.hover_pos()).filter(|p| viewport.contains(*p)).unwrap_or(viewport.center());
                    self.create_node(&kind, self.view.to_world(viewport.min, at));
                }
            }
            // Menus and editors own Escape before canvas gestures; comments consume it above.
            if escape && !self.text_focused && !egui::Popup::is_any_open(ui.ctx()) {
                self.cancel_canvas_gesture();
            }
        }
        let popup_owned = egui::Popup::is_any_open(ui.ctx());
        // Taken before navigation too: a fit or pan from the overlay also moves the view.
        let view_before = self.view;
        let layout_blocks = self.layout_overlay(ui);
        let overlay_blocks = self.navigation_overlay(ui, minimap_press) || search_blocks || layout_blocks || comments_blocks || tag_blocks;
        // Any popup owns canvas gestures, but its own card controls must still deliver edits.
        let canvas_blocks = overlay_blocks || popup_owned;
        let shift = ui.input(|i| i.modifiers.shift);
        let background = ui.interact(viewport, ui.id().with("canvas"), Sense::click_and_drag());
        if !canvas_blocks
            && background.drag_started_by(egui::PointerButton::Primary)
            && shift
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.marquee = Some(Marquee { start: self.view.to_world(viewport.min, origin), base: self.selection.clone(), cancelled: false });
        }
        let over_card = ui.input(|i| i.pointer.hover_pos()).is_some_and(|pointer| {
            // navigation_overlay refreshed actual-instance geometry for this revision above.
            // The visible-card schema cache is refreshed later and may still describe an old size.
            self.navigation.geometry.rects.values().any(|rect| self.view.rect_to_screen(viewport.min, *rect).contains(pointer))
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
        let mut pan = Vec2::ZERO;
        let mut query = None;
        let mut split_request = None;
        let mut wire_command = None;
        let mut layout_mode = None;
        // The owning context menu may invoke layout; unrelated popups have no such route.
        let layout_context_ready = self.layout_base_available(ui.ctx());
        let mut comment_target = None;
        let mut split_after = None;
        let mut tag_action = None;
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
        if let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
            let kinds = self.session.kinds();
            self.editors.retain(|id, _| canvas.nodes().contains_key(id));
            self.schemas.retain(|id| canvas.nodes().contains_key(id));
            self.selection.retain_existing(canvas);
            // Selected cards draw last, on top.
            let selected = self.selection.clone();
            self.painted.clear();
            for node in
                canvas.nodes().values().filter(|node| !selected.contains(node.id)).chain(canvas.nodes().values().filter(|node| selected.contains(node.id)))
            {
                // Cull conservatively before resolving potentially large instance schemas.
                let position = self.drag.as_ref().filter(|drag| drag.id == node.id).map_or(node.pos, |drag| drag.pos);
                let bounds = Rect::from_min_size(Pos2::new(position.x, position.y), Vec2::new(t.card_width, t.card_max_height + t.card_preview_height));
                if !self.view.rect_to_screen(viewport.min, bounds).expand(card::cull_margin(t, self.view.zoom)).intersects(viewport) {
                    if let Some(editor) = self.editors.get_mut(&node.id) {
                        editor.mark_hidden();
                    }
                    continue;
                }
                // Only cards on screen at a readable zoom ask for their image (#386).
                let image = nodecraft_engine::split::image_of(node).filter(|_| self.view.zoom >= card::BODY_MIN_ZOOM).map(|asset| {
                    let hash = asset.hash;
                    let edge = t.card_width * self.view.zoom * ui.ctx().pixels_per_point();
                    let assets = self.session.assets();
                    self.previews.request(ui.ctx(), hash, edge, || assets.get(&hash).map(<[u8]>::to_vec))
                });
                let schema = cached_schema(&mut self.schemas, &self.session, node, self.cache_clock);
                let descriptor = schema.map(|cached| cached.descriptor.as_ref());
                let editor = self.editors.entry(node.id).or_default();
                if let Some(result) = card::show(
                    ui,
                    &self.view,
                    node,
                    card::CardView {
                        descriptor,
                        visible: schema.map(|cached| &cached.visible),
                        preview_pos: self.drag.as_ref().filter(|drag| drag.id == node.id).map(|drag| drag.pos),
                        image,
                        grid: self.split_shown.filter(|(id, ..)| *id == node.id).map(|(_, rows, columns)| {
                            let full = nodecraft_engine::split::image_of(node).and_then(|asset| Some([asset.meta.width?, asset.meta.height?]));
                            (rows, columns, full)
                        }),
                    },
                    editor,
                    self.selection.contains(node.id),
                    t,
                ) {
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
                    if (!overlay_blocks || egui::Popup::is_any_open(ui.ctx()))
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
                            if nodecraft_engine::split::image_of(node).is_some() {
                                ui.menu_button("Split into grid", |ui| {
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
                                (!overlay_blocks || self.text_focused || (popup_owned && matches!(intent, Intent::Parameter { .. })))
                                    && (!canvas_blocks || !matches!(intent, Intent::Move(_)))
                            })
                            .map(|intent| (node.id, intent)),
                    );
                }
            }
            // Noodles follow this frame's painted cards (drag previews, resolved heights). They are
            // painted on the canvas layer, which stays under the card layers.
            let curves = curves(canvas, &self.painted, kinds, t);
            if !overlay_blocks && background.clicked() {
                let hit = ui.input(|i| i.pointer.interact_pos()).and_then(|p| {
                    let world = self.view.to_world(viewport.min, p);
                    let reach = NOODLE_HIT / self.view.zoom;
                    curves.iter().map(|(id, curve, _)| (*id, noodle::distance(curve, world))).filter(|(_, d)| *d <= reach).min_by(|a, b| a.1.total_cmp(&b.1))
                });
                match (hit, shift) {
                    (Some((id, _)), true) => self.selection.toggle_edge(id),
                    (Some((id, _)), false) => self.selection.only_edge(id),
                    (None, false) => self.selection.clear(),
                    (None, true) => {}
                }
            }
            // Double-click empty canvas (not a noodle) for the quick-add palette.
            if background.double_clicked()
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
            if let Some(marquee) = &mut self.marquee
                && !marquee.cancelled
                && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            {
                // Ids removed mid-drag (e.g. by undo) must not come back from the base.
                marquee.base.retain_existing(canvas);
                let world = Rect::from_two_pos(marquee.start, self.view.to_world(viewport.min, pointer));
                self.selection = marquee.base.clone();
                self.selection.extend(
                    selection::in_box(canvas, world, |node| box_geometry(&self.painted, kinds, node, world, t)),
                    curves.iter().filter(|(_, curve, _)| noodle::touches(curve, world)).map(|(id, _, _)| *id),
                );
                // Above the card layers, which are drawn over the background painter.
                ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("marquee"))).with_clip_rect(viewport).rect(
                    self.view.rect_to_screen(viewport.min, world),
                    0.0,
                    t.marquee_fill,
                    egui::Stroke::new(1.0, t.accent),
                    egui::StrokeKind::Inside,
                );
            }
        } else {
            self.selection.clear();
        }
        if let Some(action) = tag_action {
            if let Err(error) = self.apply_tag_action(action) {
                self.notice = Some(error.message);
            }
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
        for (id, intent) in intents {
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
        if !ui.input(|i| i.pointer.primary_down())
            && let Some(drag) = self.drag.take()
            && let Err(error) = self.session.dispatch("node.move", &json!({"canvas":self.canvas,"id":drag.id,"pos":drag.pos,"gesture":drag.gesture}))
        {
            self.notice = Some(error.message);
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.marquee = None;
        }
        self.view.pan_by(pan);
        let hidden_draft_notice = self
            .editors
            .values()
            .any(|editor| editor.hidden_dirty)
            .then_some("Unsaved draft in a hidden node or field. Zoom or pan back to edit it, or discard drafts.");
        if let Some(notice) = self.notice.as_deref().or(hidden_draft_notice) {
            egui::Area::new(ui.id().with("notice"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(t.card_padding, -t.card_padding))
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding).show(ui, |ui| {
                        ui.label(notice);
                        if hidden_draft_notice.is_some() && ui.button("Discard drafts").clicked() {
                            self.editors.clear();
                        }
                    });
                });
        }
        self.drop_files(ui, viewport, t);
        if let Some(mut open) = self.palette.take() {
            let entries = self.palette_entries();
            match open.show(ui.ctx(), &entries, t) {
                palette::Outcome::Open => self.palette = Some(open),
                palette::Outcome::Closed => {}
                palette::Outcome::Chosen(palette::Target::Kind(kind)) => self.create_node(&kind, open.world),
                palette::Outcome::Chosen(palette::Target::Command(id)) => match self.session.dispatch(id, &json!({})) {
                    // The report is for reading elsewhere: put it on the clipboard.
                    Ok(report) if id == "perf.report" => {
                        ui.ctx().copy_text(serde_json::to_string_pretty(&report).unwrap_or_default());
                        self.notice = Some("Performance report copied to the clipboard.".into());
                    }
                    Ok(_) => self.notice = None,
                    Err(error) => self.notice = Some(error.message),
                },
            }
        }
        if let Some(mut open) = self.turn_into.take() {
            match open.show(ui.ctx(), t) {
                turn_into::Outcome::Open => self.turn_into = Some(open),
                turn_into::Outcome::Closed => {}
                turn_into::Outcome::Chosen(kind) => self.create_connected(&kind, &open),
            }
        }
        self.navigation.finish_frame(ui.ctx());
        self.tag_focused = self.tag_filter.open && ui.ctx().memory(|m| m.focused()).is_some_and(|id| self.tag_focus_ids.contains(&id));
        self.search_focused = ui.ctx().memory(|m| m.focused()) == Some(self.search_input_id());
        self.text_focused = canvas::text_input_focused(ui.ctx());
        self.last_text_focus = self.text_focused.then(|| ui.ctx().memory(|m| m.focused())).flatten();
        self.keys_button(ui, viewport);
        let changes = self.keys.show(ui.ctx(), &mut self.session, &self.key_accounts, t);
        self.save_accounts(changes);
        ui.ctx().options_mut(|o| o.input_options.surrender_focus_on = surrender_focus_on);
    }

    /// Top-right button that opens the API keys panel.
    fn keys_button(&mut self, ui: &mut egui::Ui, viewport: Rect) {
        let t = self.theme.tokens();
        let frame = egui::Frame::NONE
            .fill(t.panel_fill)
            .stroke(egui::Stroke::new(1.0, t.panel_stroke))
            .corner_radius(t.panel_radius)
            .inner_margin(egui::Margin::symmetric(6, 4));
        egui::Area::new(ui.id().with("keys_button"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-16.0, 16.0))
            .constrain_to(viewport)
            .show(ui.ctx(), |ui| {
                frame.show(ui, |ui| {
                    if ui
                        .add(egui::Button::new(egui::RichText::new("API keys").color(t.text_muted)).frame(false))
                        .on_hover_text("Add, check and remove provider keys")
                        .clicked()
                    {
                        self.keys.open = !self.keys.open;
                    }
                });
            });
    }

    /// Keeps the remembered account names in settings in step with the keychain.
    fn save_accounts(&mut self, changes: Vec<crate::keys::AccountChange>) {
        if changes.is_empty() {
            return;
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
    /// Delete the selection (nodes and noodles) in one undo step.
    fn dispatch_delete(&mut self) {
        let (nodes, edges) = (self.selection.nodes().clone(), self.selection.edges().clone());
        self.dispatch_delete_of(&nodes, &edges);
    }

    /// Delete these nodes and noodles in one undo step, skipping ids an undo already removed.
    fn dispatch_delete_of(&mut self, nodes: &std::collections::BTreeSet<NodeId>, edges: &std::collections::BTreeSet<EdgeId>) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let nodes: Vec<NodeId> = nodes.iter().copied().filter(|id| canvas.nodes().contains_key(id)).collect();
        let edges: Vec<EdgeId> = edges.iter().copied().filter(|id| canvas.edges().contains_key(id)).collect();
        if nodes.is_empty() && edges.is_empty() {
            return;
        }
        match self.session.dispatch("canvas.delete", &json!({"canvas": self.canvas, "nodes": nodes, "edges": edges})) {
            Ok(_) => {
                self.notice = None;
                self.selection.clear();
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Paste copied nodes centred on `centre` (one undo step) and select them.
    /// The selection as a clipboard payload.
    fn selection_payload(&self) -> Option<clipboard::Payload> {
        let document = self.session.document();
        document.canvases().get(&self.canvas).and_then(|canvas| clipboard::payload(document.id(), canvas, &self.selection))
    }

    /// Whether `payload` fits what `canvas.paste` accepts (command JSON limits), with a notice if
    /// not.
    fn pasteable(&self, payload: &clipboard::Payload) -> Result<(), String> {
        let params = json!({"canvas": self.canvas, "nodes": payload.nodes, "edges": payload.edges});
        nodecraft_engine::commands::validate_json(&params)
            .map_err(|_| "This selection is too large to copy and paste back in one go (over 1 MiB of text or 16,384 values); copy fewer nodes.".to_owned())
    }

    fn paste(&mut self, payload: clipboard::Payload, centre: Pos2) {
        let t = self.theme.tokens();
        let kinds = self.session.kinds();
        let placed = clipboard::place(self.session.document(), self.canvas, payload, centre, |n| payload_rect(kinds, n, t), self.session.revision());
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

    /// Paste plain text as a Text node centred on `centre`.
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
        })
    }

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
                node.pos = nodecraft_graph::Position { x: centre.x - t.card_width * 0.5, y: centre.y - t.card_min_height * 0.5 };
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
    /// parameters right now.
    fn palette_entries(&self) -> Vec<palette::Entry> {
        let mut kinds: Vec<palette::Entry> = self
            .session
            .kinds()
            .descriptors()
            .map(|d| palette::Entry { target: palette::Target::Kind(d.id.clone()), label: d.label.clone(), shortcut: d.quick_add.clone() })
            .collect();
        kinds.sort_by(|a, b| a.label.cmp(&b.label));
        let commands = self.session.registry().specs().filter(|spec| self.session.enabled(spec.id, &json!({})).is_ok()).map(|spec| palette::Entry {
            target: palette::Target::Command(spec.id),
            label: spec.label.to_owned(),
            shortcut: spec.shortcut.map(str::to_owned),
        });
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
        // Last, so it sees any `CancelClose` from an unsaved-changes check (none in M0).
        craft_quit::on_close_request(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
    fn on_exit(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.close_storage();
        #[cfg(not(target_arch = "wasm32"))]
        // A read-only store (newer schema, unwritable folder) was reported at startup.
        if let Some(store) = &mut self.store
            && !store.is_read_only()
            && let Err(e) = store.flush()
        {
            log::warn!("settings not saved on exit: {e}");
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.theme.tokens().canvas_bg.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod action_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod order_tests;
#[cfg(test)]
mod perf;
#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod split_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod turn_into_tests;
