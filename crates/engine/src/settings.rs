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
//! The store is native only (the browser build keeps settings in memory for now, with defaults).

use serde::{Deserialize, Serialize};

mod agent;
mod projects;
pub use agent::{AgentEndpoint, AgentMode, AgentSettings};
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
#[serde(default)]
pub struct ProviderSettings {
    /// The ComfyUI server, such as `http://127.0.0.1:8188`; unset, ComfyUI models aren't set up.
    /// A key stored for the server's account (`comfyui:<host>:<port>`) is sent as a bearer token.
    pub comfyui_url: Option<String>,
    /// A random value made once per install; it keeps prompt ids of different installs apart on
    /// a shared ComfyUI server. Empty until the host first creates it.
    pub install_salt: String,
}

impl ProviderSettings {
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

impl craft_settings::Settings for NodeCraftSettings {
    const APP_ID: &'static str = "nodecraft";
    const SCHEMA_VERSION: u32 = 1;
    const PORTABLE_MARKERS: &'static [&'static str] = &["portable.txt"];

    fn migrate(from: u32, doc: &mut DocumentMut) -> Result<(), MigrateError> {
        // Version 1 is the first schema; add a step here (`from == 1` → 2) when a key is renamed
        // or reshaped, and a test with a version-1 file.
        let _ = (from, doc);
        Ok(())
    }
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
    fn peek_reads_without_touching_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent");
        assert_eq!(peek_at(&missing), NodeCraftSettings::default());
        assert!(!missing.exists(), "peeking must not create the folder");
        write_config(dir.path(), "[providers]\ncomfyui_url = \"http://127.0.0.1:8188\"\n[canvas]\nzoom = \"not a table\"\n");
        let before = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        let peeked = peek_at(dir.path());
        assert_eq!(peeked.providers.comfyui_url.as_deref(), Some("http://127.0.0.1:8188"));
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
        assert!(text.contains("schema_version = 1"), "{text}");
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
}
