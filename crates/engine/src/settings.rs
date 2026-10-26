//! App settings: `config.toml` through `craft-settings`.
//!
//! The folder is chosen by `craft-settings` (first match wins): an explicit override, the
//! `NODECRAFT_CONFIG_DIR` variable (empty means in-memory only), a `portable.txt` next to the
//! executable (settings go in `config/` beside it; the Windows portable zip ships one), then the
//! platform folder (`~/.config/nodecraft`, `~/Library/Application Support/nodecraft`,
//! `%APPDATA%\nodecraft`). Opening never fails: a damaged file is moved aside and defaults are
//! used, a bad section falls back to its defaults, and the [`LoadReport`] says what happened.
//! Hand edits, comments and unknown keys survive saves.
//!
//! Values read from the file are hostile like any other input: use the accessor methods (such as
//! [`CanvasSettings::zoom_speed`]), which clamp them, rather than the raw fields.
//!
//! This store is native only. Browser provider addresses and their salt use per-origin storage
//! through `browser_providers`; other browser app settings still use in-memory defaults.

use serde::{Deserialize, Serialize};

mod agent;
mod budgets;
mod projects;
pub use agent::{AgentEndpoint, AgentMode, AgentSettings};
pub use budgets::BudgetSettings;
pub use projects::ProjectSettings;

pub use craft_settings::{DocumentMut, LoadReport, MigrateError, SaveStatus, SettingsError, TickResult};
#[cfg(not(target_arch = "wasm32"))]
pub use craft_settings::{Location, OpenOptions};

/// The settings store for NodeCraft (native only).
#[cfg(not(target_arch = "wasm32"))]
pub type SettingsStore = craft_settings::SettingsStore<NodeCraftSettings>;

/// Everything in `config.toml`. Every struct has `#[serde(default)]`, so missing keys fall back to
/// their defaults and a newer file still loads in an older build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeCraftSettings {
    pub appearance: AppearanceSettings,
    pub canvas: CanvasSettings,
    pub keys: KeySettings,
    pub providers: ProviderSettings,
    pub projects: ProjectSettings,
    pub updates: UpdateSettings,
    pub media: MediaSettings,
    pub agent: AgentSettings,
    pub share: ShareSettings,
    pub budgets: BudgetSettings,
}

/// External tools for video (#105). NodeCraft never downloads or bundles them, and never takes
/// their paths from a project: only this file names them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaSettings {
    /// The installed `ffmpeg`, as an absolute path. Read through [`MediaSettings::tools`].
    pub ffmpeg: Option<std::path::PathBuf>,
    /// The installed `ffprobe`, as an absolute path.
    pub ffprobe: Option<std::path::PathBuf>,
}

impl MediaSettings {
    /// Both tools when both are set to absolute paths; video playback needs the pair.
    pub fn tools(&self) -> Option<(&std::path::Path, &std::path::Path)> {
        let ffmpeg = self.ffmpeg.as_deref().filter(|p| p.is_absolute())?;
        let ffprobe = self.ffprobe.as_deref().filter(|p| p.is_absolute())?;
        Some((ffmpeg, ffprobe))
    }
}

/// Checking for a newer release (#123). Only notifies: nothing is downloaded or installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Check once when the app starts. `app.check_updates` checks whenever it's asked.
    pub check: bool,
    pub channel: crate::update_check::UpdateChannel,
    /// The release the person dismissed; it isn't offered again, a newer one is.
    pub dismissed: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { check: true, channel: crate::update_check::UpdateChannel::Stable, dismissed: None }
    }
}

/// How to reach the providers that need more than a key (#409). Keys stay in the keychain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, from = "ReadProviderSettings")]
pub struct ProviderSettings {
    /// The ComfyUI servers (`[[providers.comfyui]]`, #864); none, ComfyUI models aren't set up.
    /// Read them through [`Self::comfy_servers`], which skips bad and repeated entries. A key
    /// stored for a server's account (`comfyui:<host>:<port>`) is sent to it as a bearer token.
    pub comfyui: Vec<ComfyServer>,
    /// The server cards run on unless they pick one, by name; unset or unknown, the first.
    pub comfyui_default: Option<String>,
    /// The LiteLLM proxy root, without `/v1`; its optional virtual key uses account `litellm`.
    pub litellm_url: Option<String>,
    /// Azure OpenAI resource root or `/openai/v1` base; the key uses account `azure-openai`.
    pub azure_openai_url: Option<String>,
    /// A random value made once per install; it keeps prompt ids of different installs apart on
    /// a shared ComfyUI server. Empty until the host first creates it.
    pub install_salt: String,
}

/// [`ProviderSettings`] as read. A file with no `schema_version` counts as current and isn't
/// migrated, so a hand-written one may still hold the old single `comfyui_url`: with no servers
/// listed, it becomes the one server and the default.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ReadProviderSettings {
    comfyui: Vec<ComfyServer>,
    comfyui_default: Option<String>,
    install_salt: String,
    comfyui_url: Option<String>,
    litellm_url: Option<String>,
    azure_openai_url: Option<String>,
}

impl From<ReadProviderSettings> for ProviderSettings {
    fn from(read: ReadProviderSettings) -> Self {
        let ReadProviderSettings { comfyui, comfyui_default, install_salt, comfyui_url, litellm_url, azure_openai_url } = read;
        match comfyui_url.as_deref().map(str::trim).filter(|url| !url.is_empty()) {
            Some(url) if comfyui.is_empty() => Self { litellm_url, azure_openai_url, ..Self::with_comfyui(url, &install_salt) },
            _ => Self { comfyui, comfyui_default, litellm_url, azure_openai_url, install_salt },
        }
    }
}

/// One ComfyUI server (#864).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComfyServer {
    /// What the panel and cards call it; unique among the servers.
    pub name: String,
    /// Such as `http://127.0.0.1:8188`.
    pub url: String,
    /// How many jobs one run sends it at once; unset, the scheduler's usual 4. Read through
    /// [`Self::max_jobs`].
    #[serde(deserialize_with = "lenient_jobs")]
    pub max_jobs: Option<u32>,
}

/// A hand-edited `max_jobs` that isn't a whole number in range drops the cap (or is clamped),
/// rather than resetting the whole `[providers]` section.
fn lenient_jobs<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    let jobs = match <Option<serde_json::Value> as Deserialize>::deserialize(deserializer)? {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(text)) => text.trim().parse::<i64>().ok(),
        _ => None,
    };
    Ok(jobs.map(|n| u32::try_from(n.clamp(1, i64::from(MAX_SERVER_JOBS))).unwrap_or(1)))
}

/// At most this many ComfyUI servers are used; later entries are ignored.
pub const MAX_COMFY_SERVERS: usize = 32;
/// The longest server name, in characters.
pub const MAX_SERVER_NAME: usize = 64;
/// The most jobs one run sends a server at once: the scheduler runs at most 16 jobs at once in all.
pub const MAX_SERVER_JOBS: u32 = 16;

impl ComfyServer {
    pub fn new(name: &str, url: &str) -> Self {
        Self { name: name.to_owned(), url: url.to_owned(), max_jobs: None }
    }

    /// The jobs one run may send at once, within 1..=[`MAX_SERVER_JOBS`]; `None` for the usual.
    pub fn max_jobs(&self) -> Option<usize> {
        self.max_jobs.map(|n| usize::try_from(n.clamp(1, MAX_SERVER_JOBS)).unwrap_or(1))
    }
}

/// Why `name` can't name a ComfyUI server, or `Ok` when it can: 1 to [`MAX_SERVER_NAME`]
/// characters, no control characters, no space at either end.
pub fn check_server_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("a ComfyUI server needs a name");
    }
    if name.chars().count() > MAX_SERVER_NAME {
        return Err("a ComfyUI server's name is at most 64 characters");
    }
    if name.chars().any(char::is_control) || name.trim() != name {
        return Err("a ComfyUI server's name can't hold control characters or start or end with a space");
    }
    Ok(())
}

/// A name for the server at `url`: its host and port, such as `127.0.0.1:8188`.
pub fn server_name_for(url: &str) -> String {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Never a user name or password, which belong in the keychain.
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let name: String = host.chars().filter(|c| !c.is_control() && !c.is_whitespace()).take(MAX_SERVER_NAME).collect();
    if name.is_empty() { "ComfyUI".to_owned() } else { name }
}

impl ProviderSettings {
    /// One ComfyUI server at `url`, named by [`server_name_for`] and the default.
    pub fn with_comfyui(url: &str, install_salt: &str) -> Self {
        let name = server_name_for(url);
        Self { comfyui: vec![ComfyServer::new(&name, url)], comfyui_default: Some(name), install_salt: install_salt.to_owned(), ..Self::default() }
    }

    /// The ComfyUI servers to use, in order: at most [`MAX_COMFY_SERVERS`], each with a good name
    /// and an address; a name used twice keeps its first entry.
    pub fn comfy_servers(&self) -> Vec<&ComfyServer> {
        let mut seen = std::collections::BTreeSet::new();
        self.comfyui
            .iter()
            .filter(|server| check_server_name(&server.name).is_ok() && !server.url.trim().is_empty())
            .filter(|server| seen.insert(server.name.as_str()))
            .take(MAX_COMFY_SERVERS)
            .collect()
    }

    /// The ComfyUI server called `name`.
    pub fn comfy_server(&self, name: &str) -> Option<&ComfyServer> {
        self.comfy_servers().into_iter().find(|server| server.name == name)
    }

    /// The server cards follow: the one `comfyui_default` names, or else the first.
    pub fn default_comfy(&self) -> Option<&ComfyServer> {
        let servers = self.comfy_servers();
        let named = self.comfyui_default.as_deref().and_then(|name| servers.iter().copied().find(|server| server.name == name));
        named.or_else(|| servers.first().copied())
    }

    /// Creates the install salt if there isn't one yet (32 random bytes as hex). `Ok(true)` when
    /// it was created, so the host saves the settings.
    pub fn ensure_install_salt(&mut self) -> Result<bool, nodecraft_types::Error> {
        if !self.install_salt.trim().is_empty() {
            return Ok(false);
        }
        self.install_salt = crate::execution_identity::nonce()?.iter().map(|b| format!("{b:02x}")).collect();
        Ok(true)
    }
}

/// Sharing a project live (#698).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareSettings {
    /// The relay `share.start` shares through when it isn't given one, such as
    /// `ws://relay.example:7070`; `nodecraft-cli relay` runs one.
    pub relay: Option<String>,
}

impl craft_settings::Settings for NodeCraftSettings {
    const APP_ID: &'static str = "nodecraft";
    const SCHEMA_VERSION: u32 = 2;
    const PORTABLE_MARKERS: &'static [&'static str] = &["portable.txt"];

    fn migrate(from: u32, doc: &mut DocumentMut) -> Result<(), MigrateError> {
        // Add a step for each version, with a test reading a file of the version before.
        if from == 1 {
            migrate_comfy_servers(doc).map_err(|message| MigrateError::new(from, message))?;
        }
        Ok(())
    }
}

/// Version 1 → 2 (#864): `[providers] comfyui_url` becomes the one `[[providers.comfyui]]`
/// server, named by its host and port, and the default.
fn migrate_comfy_servers(doc: &mut DocumentMut) -> Result<(), String> {
    let Some(providers) = doc.get_mut("providers").and_then(|item| item.as_table_like_mut()) else {
        return Ok(());
    };
    // The comment above the old key stays above the new ones.
    let comment = providers.key_mut("comfyui_url").map(|key| key.leaf_decor().clone());
    let Some(url) = providers.remove("comfyui_url") else {
        return Ok(());
    };
    let Some(address) = url.as_str().map(str::trim).filter(|address| !address.is_empty()) else {
        return Ok(());
    };
    // The entry is parsed from text, the only way to build TOML here; the name holds no quote or
    // backslash, and the address moves over as it was written.
    let name: String = server_name_for(address).chars().filter(|c| !matches!(c, '"' | '\\')).collect();
    let name = if name.is_empty() { "ComfyUI".to_owned() } else { name };
    let mut made: DocumentMut = format!("comfyui_default = \"{name}\"\n[[comfyui]]\nname = \"{name}\"\n")
        .parse()
        .map_err(|_| "the ComfyUI server's name can't be written".to_owned())?;
    let server = made.get_mut("comfyui").and_then(|servers| servers.get_mut(0)).and_then(|server| server.as_table_like_mut());
    let Some(server) = server else {
        return Err("the ComfyUI server can't be written".into());
    };
    server.insert("url", url);
    for key in ["comfyui_default", "comfyui"] {
        if let Some(item) = made.remove(key) {
            providers.insert(key, item);
        }
    }
    if let (Some(comment), Some(mut key)) = (comment, providers.key_mut("comfyui_default")) {
        *key.leaf_decor_mut() = comment;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: Theme,
}

/// Colour theme of the whole app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// What a plain scroll (mouse wheel, two-finger trackpad swipe) does on the canvas.
/// Pinch and ⌘/Ctrl-scroll always zoom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollAction {
    /// Scroll pans the canvas (trackpad-first, the default).
    #[default]
    Pan,
    /// Vertical scroll zooms around the pointer (mouse-wheel users); horizontal scroll still pans.
    Zoom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasSettings {
    pub scroll: ScrollAction,
    /// Multiplier on pinch, ⌘-scroll and scroll-to-zoom speed. Read it through
    /// [`CanvasSettings::zoom_speed`], which clamps it.
    pub zoom_speed: f32,
}

impl Default for CanvasSettings {
    fn default() -> Self {
        Self { scroll: ScrollAction::default(), zoom_speed: 1.0 }
    }
}

impl CanvasSettings {
    pub const MIN_ZOOM_SPEED: f32 = 0.25;
    pub const MAX_ZOOM_SPEED: f32 = 4.0;

    /// The zoom speed from the file, clamped to a usable range; 1.0 if it is not a number.
    pub fn zoom_speed(&self) -> f32 {
        if self.zoom_speed.is_finite() { self.zoom_speed.clamp(Self::MIN_ZOOM_SPEED, Self::MAX_ZOOM_SPEED) } else { 1.0 }
    }
}

/// Which provider accounts have a key in the keychain: names only, never keys. Kept here so the
/// keys panel can list them without reading every key at startup (each read can show an unlock
/// prompt).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeySettings {
    /// Read through [`KeySettings::accounts`], which drops invalid and duplicate names.
    pub accounts: Vec<String>,
}

impl KeySettings {
    /// Most accounts listed.
    pub const MAX_ACCOUNTS: usize = 64;

    /// The valid account names from the file, without duplicates, in order, at most
    /// [`Self::MAX_ACCOUNTS`].
    pub fn accounts(&self) -> Vec<crate::secrets::Account> {
        let mut out: Vec<crate::secrets::Account> = Vec::new();
        for name in &self.accounts {
            if out.len() >= Self::MAX_ACCOUNTS {
                break;
            }
            if let Ok(account) = crate::secrets::Account::new(name)
                && !out.contains(&account)
            {
                out.push(account);
            }
        }
        out
    }

    /// Adds `account` to the list (no-op if listed or the list is full). Like [`Self::forget`],
    /// it rewrites the list from [`Self::accounts`], so invalid, duplicate and surplus names from a
    /// hand-edited file are dropped.
    pub fn remember(&mut self, account: &crate::secrets::Account) {
        let known = self.accounts();
        if !known.contains(account) && known.len() < Self::MAX_ACCOUNTS {
            self.accounts = known.iter().map(|a| a.as_str().to_owned()).chain([account.as_str().to_owned()]).collect();
        }
    }

    /// Removes `account` from the list.
    pub fn forget(&mut self, account: &crate::secrets::Account) {
        self.accounts = self.accounts().into_iter().filter(|a| a != account).map(|a| a.as_str().to_owned()).collect();
    }
}

/// The settings folder the app uses, resolved without opening, creating or touching anything (other
/// tools, such as the CLI, use it to find files the app writes there). `None` when settings are in
/// memory only. Portable mode is judged by this executable's folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn settings_dir() -> Option<std::path::PathBuf> {
    use craft_settings::Settings;
    let portable = craft_settings::Portable { markers: NodeCraftSettings::PORTABLE_MARKERS, dir: NodeCraftSettings::PORTABLE_DIR };
    craft_settings::paths::resolve(NodeCraftSettings::APP_ID, &portable, &Location::Auto, &craft_settings::Probe::default()).dir
}

/// The largest `config.toml` [`peek`] reads; a bigger one is ignored (defaults are used).
#[cfg(not(target_arch = "wasm32"))]
const PEEK_LIMIT: u64 = 4 << 20;

/// The app's settings as saved, read without opening a store: nothing is created, locked,
/// migrated on disk or moved aside, so other tools (the CLI) can read them while the app runs.
/// Defaults when settings are in memory only, or the file is missing, too big or unreadable; a bad
/// section falls back to its defaults, as it does in the app.
#[cfg(not(target_arch = "wasm32"))]
pub fn peek() -> NodeCraftSettings {
    settings_dir().map_or_else(NodeCraftSettings::default, |dir| peek_at(&dir))
}

/// [`peek`] in `dir`.
#[cfg(not(target_arch = "wasm32"))]
pub fn peek_at(dir: &std::path::Path) -> NodeCraftSettings {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(dir.join(craft_settings::store::CONFIG_FILE)) else {
        return NodeCraftSettings::default();
    };
    let mut text = String::new();
    if file.take(PEEK_LIMIT.saturating_add(1)).read_to_string(&mut text).is_err() || u64::try_from(text.len()).map_or(true, |n| n > PEEK_LIMIT) {
        return NodeCraftSettings::default();
    }
    let Ok(mut doc) = craft_settings::document::parse(&text) else {
        return NodeCraftSettings::default();
    };
    // Upgraded in memory only; a failed step leaves a half-upgraded document, so use defaults.
    if craft_settings::migrate::upgrade::<NodeCraftSettings>(&mut doc).is_err() {
        return NodeCraftSettings::default();
    }
    craft_settings::document::deserialize_sections::<NodeCraftSettings>(&doc).0
}

/// Makes the install salt if `store` has none, saving it at once: ComfyUI prompt ids must stay
/// the same across restarts, so a salt is never made unless it is saved. On a read-only store
/// none is made (and ComfyUI reports itself not set up).
#[cfg(not(target_arch = "wasm32"))]
pub fn ensure_install_salt(store: &mut SettingsStore) -> Result<(), nodecraft_types::Error> {
    use nodecraft_types::{Error, ErrorKind};
    if !store.get().providers.install_salt.trim().is_empty() {
        return Ok(());
    }
    if store.is_read_only() {
        return Err(Error::new(ErrorKind::Unsupported, "settings can't be saved, so no install salt was made; ComfyUI stays off"));
    }
    let mut providers = store.get().providers.clone();
    providers.ensure_install_salt()?;
    store.update(|settings| settings.providers = providers);
    if store.flush().is_err() {
        store.update(|settings| settings.providers.install_salt.clear());
        return Err(Error::new(ErrorKind::Internal, "the install salt couldn't be saved; ComfyUI stays off"));
    }
    Ok(())
}

/// Opens the settings for `location`. Never fails; problems are in the report.
#[cfg(not(target_arch = "wasm32"))]
pub fn open(location: Location) -> (SettingsStore, LoadReport) {
    SettingsStore::open(location)
}

/// One line per problem in `report`, for the log and a notice to the user. Empty when all is well.
pub fn describe_problems(report: &LoadReport) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(moved) = &report.broken_moved_to {
        out.push(format!("settings file was damaged and moved to {}; using defaults", moved.display()));
    }
    for e in &report.section_errors {
        let message = e.message.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push(format!("settings section [{}] was invalid and reset to defaults: {message}", e.section));
    }
    if let Some(why) = &report.read_only {
        out.push(format!("settings will not be saved: {why}"));
    }
    out
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use craft_settings::Source;
    use std::path::Path;

    fn open_in(dir: &Path) -> (SettingsStore, LoadReport) {
        open(Location::Dir(dir.to_path_buf()))
    }

    fn write_config(dir: &Path, text: &str) {
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    #[test]
    fn a_mistyped_budget_cap_in_the_file_blocks_rather_than_lifting_every_cap() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[budgets]\nper_call_usd_cents = 50.0\nsession_usd_cents = 700\nproject_usd_cents = 12\ntotal_usd_cents = \"9\"\n");
        let (store, _) = open_in(dir.path());
        let caps = &store.get().budgets;
        assert_eq!((caps.per_call_usd_cents, caps.session_usd_cents, caps.project_usd_cents, caps.total_usd_cents), (Some(0), Some(700), Some(12), Some(0)));
    }

    #[test]
    fn peek_reads_without_touching_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent");
        assert_eq!(peek_at(&missing), NodeCraftSettings::default());
        assert!(!missing.exists(), "peeking must not create the folder");
        write_config(dir.path(), "[providers]\ncomfyui_url = \"http://127.0.0.1:8188\"\n[canvas]\nzoom = \"not a table\"\n");
        let before = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        let peeked = peek_at(dir.path());
        assert_eq!(peeked.providers.default_comfy().map(|server| server.url.as_str()), Some("http://127.0.0.1:8188"));
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config.toml")], "nothing is moved aside, backed up or locked");
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), before);
        write_config(dir.path(), "this is [not toml");
        assert_eq!(peek_at(dir.path()), NodeCraftSettings::default());
        assert!(dir.path().join("config.toml").exists(), "a damaged file stays where it is");
    }

    #[test]
    fn first_run_uses_defaults_and_writes_the_file_on_flush() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, report) = open_in(dir.path());
        assert_eq!(report.source, Source::Defaults);
        assert!(describe_problems(&report).is_empty());
        assert_eq!(store.get(), &NodeCraftSettings::default());
        store.flush().unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(text.contains("schema_version = 2"), "{text}");
        assert!(text.contains("theme = \"dark\""), "{text}");
        assert!(text.contains("scroll = \"pan\""), "{text}");
    }

    #[test]
    fn changes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = open_in(dir.path());
        assert!(store.update(|s| {
            s.appearance.theme = Theme::Light;
            s.canvas.scroll = ScrollAction::Zoom;
            s.canvas.zoom_speed = 2.0;
        }));
        store.flush().unwrap();
        let (again, report) = open_in(dir.path());
        assert_eq!(report.source, Source::File);
        assert_eq!(again.get().appearance.theme, Theme::Light);
        assert_eq!(again.get().canvas.scroll, ScrollAction::Zoom);
        assert_eq!(again.get().canvas.zoom_speed(), 2.0);
    }

    #[test]
    fn video_tools_need_two_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = dir.path().join("ffmpeg");
        let ffprobe = dir.path().join("ffprobe");
        write_config(dir.path(), &format!("schema_version = 1\n[media]\nffmpeg = '{}'\nffprobe = '{}'\n", ffmpeg.display(), ffprobe.display()));
        let (store, report) = open_in(dir.path());
        assert!(describe_problems(&report).is_empty(), "{report:?}");
        assert_eq!(store.get().media.tools(), Some((ffmpeg.as_path(), ffprobe.as_path())));
        let relative = MediaSettings { ffmpeg: Some("ffmpeg".into()), ffprobe: Some(ffprobe.clone()) };
        assert_eq!(relative.tools(), None);
        assert_eq!(MediaSettings { ffmpeg: Some(ffmpeg), ffprobe: None }.tools(), None);
    }

    #[test]
    fn hand_edits_and_unknown_keys_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schema_version = 1\n# my comment\n[appearance]\ntheme = \"light\"\nfuture_key = 3\n\n[plugins]\nx = true\n");
        let (mut store, report) = open_in(dir.path());
        assert!(describe_problems(&report).is_empty(), "{report:?}");
        assert_eq!(store.get().appearance.theme, Theme::Light);
        store.update(|s| s.canvas.scroll = ScrollAction::Zoom);
        store.flush().unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        for kept in ["# my comment", "future_key = 3", "[plugins]", "scroll = \"zoom\""] {
            assert!(text.contains(kept), "{kept} missing from:\n{text}");
        }
    }

    #[test]
    fn a_damaged_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "this is [[[ not toml");
        let (store, report) = open_in(dir.path());
        assert_eq!(store.get(), &NodeCraftSettings::default());
        let moved = report.broken_moved_to.clone().expect("damaged file moved aside");
        assert!(moved.exists());
        let problems = describe_problems(&report);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("damaged"), "{problems:?}");
    }

    #[test]
    fn a_bad_section_falls_back_without_losing_the_others() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schema_version = 1\n[appearance]\ntheme = \"purple\"\n[canvas]\nscroll = \"zoom\"\n");
        let (store, report) = open_in(dir.path());
        assert_eq!(store.get().appearance.theme, Theme::Dark);
        assert_eq!(store.get().canvas.scroll, ScrollAction::Zoom);
        assert_eq!(report.section_errors.len(), 1);
        let problems = describe_problems(&report);
        assert!(problems[0].contains("[appearance]"));
        assert!(!problems[0].contains('\n'), "one line per problem: {problems:?}");
    }

    #[test]
    fn hostile_zoom_speed_is_clamped() {
        let speed = |v: f32| CanvasSettings { zoom_speed: v, ..CanvasSettings::default() }.zoom_speed();
        assert_eq!(speed(1.5), 1.5);
        assert_eq!(speed(1e9), CanvasSettings::MAX_ZOOM_SPEED);
        assert_eq!(speed(-3.0), CanvasSettings::MIN_ZOOM_SPEED);
        assert_eq!(speed(0.0), CanvasSettings::MIN_ZOOM_SPEED);
        assert_eq!(speed(f32::NAN), 1.0);
        assert_eq!(speed(f32::INFINITY), 1.0);

        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schema_version = 1\n[canvas]\nzoom_speed = nan\n");
        let (store, _) = open_in(dir.path());
        assert_eq!(store.get().canvas.zoom_speed(), 1.0);
    }

    #[test]
    fn a_newer_schema_is_read_but_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schema_version = 99\n[appearance]\ntheme = \"light\"\n");
        let (mut store, report) = open_in(dir.path());
        assert_eq!(store.get().appearance.theme, Theme::Light);
        assert!(report.read_only.is_some(), "a file from a newer build is not overwritten: {report:?}");
        assert!(store.flush().is_err());
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(text.starts_with("schema_version = 99"));
    }

    #[test]
    fn portable_marker_puts_settings_beside_the_executable() {
        let exe = tempfile::tempdir().unwrap();
        std::fs::write(exe.path().join("portable.txt"), "").unwrap();
        let probe =
            craft_settings::Probe { env: Box::new(|_| None), exe_dir: Some(exe.path().to_path_buf()), platform_config_dir: Some(exe.path().join("platform")) };
        let (store, report) = SettingsStore::open_with(OpenOptions::new(Location::Auto).probe(probe));
        assert_eq!(store.dir(), Some(exe.path().join("config").as_path()), "{report:?}");
    }

    #[test]
    fn config_dir_variable_and_isolation() {
        let target = tempfile::tempdir().unwrap();
        let t = target.path().to_path_buf();
        let probe = craft_settings::Probe {
            env: Box::new(move |name| (name == "NODECRAFT_CONFIG_DIR").then(|| t.clone().into_os_string())),
            exe_dir: None,
            platform_config_dir: None,
        };
        let (store, _) = SettingsStore::open_with(OpenOptions::new(Location::Auto).probe(probe));
        assert_eq!(store.dir(), Some(target.path()));

        let probe =
            craft_settings::Probe { env: Box::new(|name| (name == "NODECRAFT_CONFIG_DIR").then(Default::default)), exe_dir: None, platform_config_dir: None };
        let (store, _) = SettingsStore::open_with(OpenOptions::new(Location::Auto).probe(probe));
        assert!(!store.is_persistent(), "an empty NODECRAFT_CONFIG_DIR keeps settings in memory");
    }

    #[test]
    fn key_accounts_are_names_only_and_bounded() {
        let mut keys = KeySettings { accounts: vec!["openai".into(), "OpenAI".into(), "openai".into(), "comfyui:gpu:8188".into(), "has space".into()] };
        let names: Vec<String> = keys.accounts().iter().map(|a| a.as_str().to_owned()).collect();
        assert_eq!(names, ["openai", "comfyui:gpu:8188"], "invalid and duplicate names are dropped");
        let anthropic = crate::secrets::Account::new("anthropic").unwrap();
        keys.remember(&anthropic);
        keys.remember(&anthropic);
        assert_eq!(keys.accounts().len(), 3);
        keys.forget(&anthropic);
        assert_eq!(keys.accounts().len(), 2);
        let mut many = KeySettings { accounts: (0..100).map(|i| format!("p{i}")).collect() };
        assert_eq!(many.accounts().len(), KeySettings::MAX_ACCOUNTS);
        many.remember(&anthropic);
        assert!(!many.accounts().contains(&anthropic), "the list is full");
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schema_version = 1\n[keys]\naccounts = [\"openai\"]\n");
        let (store, _) = open_in(dir.path());
        assert_eq!(store.get().keys.accounts().len(), 1);
    }

    #[test]
    fn the_install_salt_is_made_once_and_kept() {
        let mut providers = ProviderSettings::default();
        assert!(providers.ensure_install_salt().unwrap());
        let salt = providers.install_salt.clone();
        assert_eq!(salt.len(), 64);
        assert!(!providers.ensure_install_salt().unwrap());
        assert_eq!(providers.install_salt, salt);
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = open_in(dir.path());
        assert!(store.update(|s| s.providers = providers.clone()));
        store.flush().unwrap();
        assert_eq!(open_in(dir.path()).0.get().providers, providers);
        let old = tempfile::tempdir().unwrap();
        write_config(old.path(), "schema_version = 1\n[keys]\naccounts = [\"openai\"]\n");
        assert_eq!(open_in(old.path()).0.get().providers, ProviderSettings::default(), "a file without [providers] still loads");
        // The store-level helper saves at once (no deferred write to lose in a crash).
        let fresh = tempfile::tempdir().unwrap();
        let (mut store, _) = open_in(fresh.path());
        ensure_install_salt(&mut store).unwrap();
        let salt = store.get().providers.install_salt.clone();
        assert_eq!(salt.len(), 64);
        let on_disk = std::fs::read_to_string(fresh.path().join("config.toml")).unwrap();
        assert!(on_disk.contains(&salt), "saved before returning");
        ensure_install_salt(&mut store).unwrap();
        assert_eq!(store.get().providers.install_salt, salt);
    }

    /// #864: a version-1 file's one ComfyUI address becomes the default of a list of servers.
    #[test]
    fn the_comfyui_address_becomes_the_default_server() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schema_version = 1\n[providers]\n# mine\ncomfyui_url = 'http://gpu-box:8188/'\ninstall_salt = \"s\"\n");
        let (mut store, report) = open_in(dir.path());
        assert_eq!(report.migrated_from, Some(1), "{report:?}");
        let providers = store.get().providers.clone();
        assert_eq!(providers.comfyui, [ComfyServer::new("gpu-box:8188", "http://gpu-box:8188/")]);
        assert_eq!(providers.default_comfy().map(|server| server.name.as_str()), Some("gpu-box:8188"));
        assert_eq!(providers.install_salt, "s");
        store.flush().unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        // Arrays are rewritten whole on saving, so the list comes back inline.
        assert!(!text.contains("comfyui_url") && text.contains("comfyui = [") && text.contains("# mine\ncomfyui_default"), "{text}");
        let (store, _) = open_in(dir.path());
        assert_eq!(store.get().providers, providers, "reads back the same");

        // No address, or a blank one, leaves no server.
        for file in ["schema_version = 1\n[providers]\ninstall_salt = \"s\"\n", "schema_version = 1\n[providers]\ncomfyui_url = ' '\n"] {
            let dir = tempfile::tempdir().unwrap();
            write_config(dir.path(), file);
            let (store, report) = open_in(dir.path());
            assert!(report.section_errors.is_empty(), "{report:?}");
            assert!(store.get().providers.comfyui.is_empty());
        }
    }

    /// A hand-edited job cap that's out of range or not a number never resets `[providers]`.
    #[test]
    fn a_bad_job_cap_is_clamped_or_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let servers = [("a", "-1"), ("b", "\"3\""), ("c", "999"), ("d", "true"), ("e", "99999999999")]
            .map(|(name, jobs)| format!("[[providers.comfyui]]\nname = \"{name}\"\nurl = \"http://{name}:1\"\nmax_jobs = {jobs}\n"));
        write_config(dir.path(), &format!("schema_version = 2\n[providers]\ninstall_salt = \"s\"\n{}", servers.concat()));
        let (store, report) = open_in(dir.path());
        assert!(report.section_errors.is_empty(), "{report:?}");
        let caps: Vec<_> = store.get().providers.comfyui.iter().map(|server| server.max_jobs).collect();
        assert_eq!(caps, [Some(1), Some(3), Some(MAX_SERVER_JOBS), None, Some(MAX_SERVER_JOBS)]);
        assert_eq!(store.get().providers.install_salt, "s");
    }

    #[test]
    fn bad_or_repeated_servers_are_skipped_and_the_first_is_the_default() {
        let mut settings = ProviderSettings {
            comfyui: vec![
                ComfyServer::new("", "http://a:1"),
                ComfyServer::new("local", "http://127.0.0.1:8188"),
                ComfyServer::new("gpu", " "),
                ComfyServer::new("local", "http://other:8188"),
                ComfyServer { max_jobs: Some(0), ..ComfyServer::new("gpu", "http://gpu:8188") },
                ComfyServer::new(" padded", "http://p:1"),
            ],
            ..ProviderSettings::default()
        };
        let names: Vec<&str> = settings.comfy_servers().iter().map(|server| server.name.as_str()).collect();
        assert_eq!(names, ["local", "gpu"]);
        assert_eq!(settings.comfy_server("local").map(|server| server.url.as_str()), Some("http://127.0.0.1:8188"));
        assert_eq!(settings.comfy_server("gpu").and_then(ComfyServer::max_jobs), Some(1), "at least one job");
        assert_eq!(settings.default_comfy().map(|server| server.name.as_str()), Some("local"));
        settings.comfyui_default = Some("gpu".into());
        assert_eq!(settings.default_comfy().map(|server| server.name.as_str()), Some("gpu"));
        settings.comfyui_default = Some("gone".into());
        assert_eq!(settings.default_comfy().map(|server| server.name.as_str()), Some("local"));
        assert_eq!(server_name_for("https://me:pw@box.example:443/comfy"), "box.example:443", "no credentials in a name");
        assert_eq!(server_name_for("::"), "::");
        assert_eq!(server_name_for(""), "ComfyUI");
    }

    #[test]
    fn azure_endpoint_survives_settings_roundtrip_and_legacy_comfy_migration() {
        for legacy in ["", "comfyui_url = 'http://localhost:8188'\n"] {
            let dir = tempfile::tempdir().unwrap();
            write_config(dir.path(), &format!("[providers]\n{legacy}azure_openai_url = 'https://demo.openai.azure.com/openai/v1'\n"));
            let (store, report) = open_in(dir.path());
            assert!(report.section_errors.is_empty(), "{report:?}");
            let settings = store.get().providers.clone();
            assert_eq!(settings.azure_openai_url.as_deref(), Some("https://demo.openai.azure.com/openai/v1"));
            let restored: ProviderSettings = serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
            assert_eq!(restored, settings);
        }
    }
}
