//! One build directory shared by several checkouts (#1033), opted into with
//! `NODECRAFT_SHARED_TARGET`.
//!
//! Every worktree's own `target/` costs about 15 GB, most of it third-party dependencies that are
//! the same in every checkout. Pointing two checkouts at one target dir is unsafe on its own: Cargo
//! names a workspace crate's artifacts the same in every checkout and judges freshness by file
//! times, so one checkout can link another's stale code. xtask makes the sharing safe:
//!
//! - an exclusive lock on the directory for the whole xtask command, so builds from different
//!   checkouts queue instead of interleaving;
//! - a stamp naming the checkout that built last. When another checkout takes the directory, the
//!   fingerprints of the workspace crates (and only those) are deleted, so Cargo rebuilds the
//!   NodeCraft crates in place and reuses every third-party dependency.
//!
//! The cost is a rebuild of the workspace crates whenever the building checkout changes, and
//! waiting while another checkout holds the lock.

use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The opt-in: an absolute path to the shared build directory.
pub const ENV: &str = "NODECRAFT_SHARED_TARGET";
/// Identifies a nested invocation that inherited the outer Cargo target environment.
const INHERITED: &str = "NODECRAFT_SHARED_TARGET_HELD";
/// The checkout that last built in the directory.
const STAMP: &str = ".nodecraft-checkout";
const STAMP_PENDING: &str = ".nodecraft-checkout.pending";
const MAX_STAMP_BYTES: u64 = 64 * 1024;
/// Durable ownership of a dedicated cache. The old in-directory lock only identifies legacy caches.
const OWNER: &str = ".nodecraft-shared-target";
const LEGACY_LOCK: &str = ".nodecraft-lock";
/// A per-command marker survives interrupted wrappers and ambiguous child exits.
const ACTIVE: &str = ".nodecraft-build-active";
/// How often to retry when the lock file was replaced under a waiter.
const LOCK_ATTEMPTS: usize = 8;

/// The configured shared directory.
#[derive(Clone, Debug)]
pub struct Shared {
    /// Absolute, as given: what cargo is told. Not canonicalised, so Windows tools never see a
    /// `\\?\` path.
    pub path: PathBuf,
    /// Canonical, for comparisons only.
    canonical: PathBuf,
    /// Quoted Cargo config value for the exact routed path.
    config_value: String,
}

/// The directory child Cargo commands use once the wrapper owns a lease.
static ROUTE: OnceLock<String> = OnceLock::new();
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Holds a command's lease. Only a known completion clears its active marker.
pub struct Guard {
    marker: PathBuf,
    _lock: File,
}

impl Guard {
    pub fn finish(self, result: Result<(), String>) -> Result<(), String> {
        let cleanup = if INTERRUPTED.load(Ordering::Relaxed) {
            Err("shared Cargo child completion was uncertain. The shared cache needs recovery".into())
        } else {
            fs::remove_file(&self.marker).map_err(|e| format!("remove {}: {e}", self.marker.display()))
        };
        match (result, cleanup) {
            (Err(command), Err(cleanup)) => Err(format!("{command}\n{cleanup}")),
            (command, cleanup) => command.and(cleanup),
        }
    }
}

pub fn inherited() -> bool {
    std::env::var_os(INHERITED).is_some()
}

pub fn interrupted_child() {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// The configured shared directory, or `None` when the variable is unset or empty.
pub fn configured() -> Result<Option<Shared>, String> {
    if inherited() {
        return Ok(None);
    }
    let Some(value) = std::env::var_os(ENV).filter(|v| !v.is_empty()) else { return Ok(None) };
    let given = PathBuf::from(value);
    if !given.is_absolute() {
        return Err(format!("{ENV} must be an absolute path"));
    }
    let path = std::path::absolute(&given).map_err(|e| format!("resolve {ENV} directory: {e}"))?;
    let name = path.file_name().ok_or("shared target needs a directory name")?;
    let parent = path.parent().ok_or("shared target needs an existing parent directory")?;
    let parent = parent.canonicalize().map_err(|e| format!("resolve shared target parent: {e}"))?;
    let canonical = parent.join(name);
    #[cfg(not(windows))]
    let path = canonical.clone();
    canonical.to_str().ok_or("shared target path must be valid UTF-8")?;
    let config_value = cargo_config_value(&path)?;
    check_directory(&canonical)?;
    let shared = Shared { path, canonical, config_value };
    // The outer Cargo builds xtask before xtask can take the lock.
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
        if std::env::var_os(var).filter(|v| !v.is_empty()).is_some_and(|own| inside(Path::new(&own), &shared.canonical)) {
            return Err(format!("set {ENV} to the shared directory. xtask itself must build outside it"));
        }
    }
    if std::env::current_exe().is_ok_and(|exe| inside(&exe, &shared.canonical)) {
        return Err(format!(
            "xtask was built inside {ENV}, outside its lock (check CARGO_TARGET_DIR and build.target-dir). Build xtask in the worktree's own target/"
        ));
    }
    Ok(Some(shared))
}

fn inside(path: &Path, dir: &Path) -> bool {
    path.canonicalize().is_ok_and(|path| path.starts_with(dir))
}

/// Locks the directory, invalidates another checkout's workspace artifacts, and routes every later
/// [`crate::cargo`] command into it.
pub fn acquire(root: &Path, shared: &Shared, members: &BTreeSet<String>) -> Result<Guard, String> {
    let lock = lock(root, shared)?;
    prepare(root, &shared.canonical, members)?;
    let marker = shared.canonical.join(ACTIVE);
    File::create_new(&marker).map_err(|e| format!("create {}: {e}", marker.display()))?;
    ROUTE.set(shared.config_value.clone()).map_err(|_| "shared target already configured")?;
    eprintln!("shared target: {}", shared.path.display());
    Ok(Guard { marker, _lock: lock })
}

fn stamp_of(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn read_stamp(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join(STAMP);
    let Some(meta) = metadata(&path)? else { return Ok(None) };
    if !meta.file_type().is_file() || meta.len() > MAX_STAMP_BYTES {
        return Err(format!("invalid checkout stamp: {}", path.display()));
    }
    let file = File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut content = String::new();
    file.take(MAX_STAMP_BYTES + 1).read_to_string(&mut content).map_err(|e| format!("read {}: {e}", path.display()))?;
    if content.len() as u64 > MAX_STAMP_BYTES {
        return Err(format!("checkout stamp is too large: {}", path.display()));
    }
    Ok(Some(content))
}

/// Initializes an empty directory and invalidates another checkout's workspace artifacts.
fn prepare(root: &Path, dir: &Path, members: &BTreeSet<String>) -> Result<(), String> {
    if !check_directory(dir)? {
        fs::create_dir(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    if !owned(dir)? {
        let mut entries = fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        if entries.next().transpose().map_err(|e| format!("read {}: {e}", dir.display()))?.is_some() {
            return Err(format!("shared target must be new or empty before first use: {}", dir.display()));
        }
        let marker = dir.join(OWNER);
        let content = ownership(dir)?;
        let mut file = File::create_new(&marker).map_err(|e| format!("create {}: {e}", marker.display()))?;
        file.write_all(content.as_bytes()).and_then(|()| file.sync_all()).map_err(|e| format!("write {}: {e}", marker.display()))?;
    }
    check_inactive(dir)?;
    let checkout = stamp_of(root);
    let pending = dir.join(STAMP_PENDING);
    if metadata(&pending)?.is_some() {
        return Err("incomplete checkout stamp. Use shared --clean to reset the cache".into());
    }
    let last = read_stamp(dir)?;
    if last.as_deref() != Some(checkout.as_str()) {
        let removed = invalidate(dir, members)?;
        if removed > 0 {
            eprintln!(
                "shared target: last built by {}, rebuilding the workspace crates ({removed} entries cleared)",
                last.as_deref().unwrap_or("an unknown checkout")
            );
        }
        // Only after a complete invalidation: a failed one is retried next time.
        let mut file = File::create_new(&pending).map_err(|e| format!("create shared target stamp: {e}"))?;
        file.write_all(checkout.as_bytes()).and_then(|()| file.sync_all()).map_err(|e| format!("write shared target stamp: {e}"))?;
        drop(file);
        fs::rename(pending, dir.join(STAMP)).map_err(|e| format!("install shared target stamp: {e}"))?;
    }
    Ok(())
}

fn metadata(path: &Path) -> Result<Option<Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("inspect {}: {e}", path.display())),
    }
}

fn check_directory(dir: &Path) -> Result<bool, String> {
    let Some(meta) = metadata(dir)? else { return Ok(false) };
    if !meta.file_type().is_dir() || dir.canonicalize().map_err(|e| format!("resolve {}: {e}", dir.display()))? != dir {
        return Err(format!("shared target must be a real directory at its canonical path: {}", dir.display()));
    }
    Ok(true)
}

fn ownership(dir: &Path) -> Result<String, String> {
    let path = dir.to_str().ok_or("shared target path must be valid UTF-8")?;
    Ok(format!("NodeCraft shared target v1\n{path}\n"))
}

fn owned(dir: &Path) -> Result<bool, String> {
    let path = dir.join(OWNER);
    let Some(meta) = metadata(&path)? else { return Ok(false) };
    let expected = ownership(dir)?;
    if meta.file_type().is_file() && meta.len() == expected.len() as u64 {
        let file = File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut content = Vec::new();
        file.take(expected.len() as u64 + 1).read_to_end(&mut content).map_err(|e| format!("read {}: {e}", path.display()))?;
        if content == expected.as_bytes() {
            return Ok(true);
        }
    }
    Err(format!("unrecognized shared target ownership marker: {}", path.display()))
}

fn check_inactive(dir: &Path) -> Result<(), String> {
    if metadata(&dir.join(ACTIVE))?.is_some() {
        return Err(format!("shared target {} was interrupted. Stop any orphaned Cargo processes before recovering the cache", dir.display()));
    }
    Ok(())
}

/// Removes an owned cache while retaining the permanent sibling lock.
pub fn clean(root: &Path, shared: &Shared) -> Result<(), String> {
    let dir = &shared.canonical;
    let _lock = lock(root, shared)?;
    if !check_directory(dir)? {
        return Ok(());
    }
    if !owned(dir)? {
        return Err(format!("refusing to remove an unowned directory: {}", dir.display()));
    }
    check_inactive(dir)?;
    remove(dir)?;
    println!("shared target: cleaned {}", shared.path.display());
    Ok(())
}

fn remove(path: &Path) -> Result<(), String> {
    let kind = fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    let result = if kind.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    result.map_err(|e| format!("remove {}: {e}", path.display()))
}

fn sibling_lock(dir: &Path) -> Option<PathBuf> {
    let mut name = dir.file_name()?.to_os_string();
    name.push(".nodecraft-shared.lock");
    Some(dir.with_file_name(name))
}

fn check_ancestors(dir: &Path) -> Result<(), String> {
    for ancestor in dir.ancestors().skip(1) {
        let reserved = match sibling_lock(ancestor) {
            Some(lock) => metadata(&lock)?.is_some(),
            None => false,
        };
        if reserved || metadata(&ancestor.join(OWNER))?.is_some() || metadata(&ancestor.join(LEGACY_LOCK))?.is_some() {
            return Err(format!("shared target cannot be nested inside a reserved shared target: {}", ancestor.display()));
        }
    }
    Ok(())
}

fn lock(root: &Path, shared: &Shared) -> Result<File, String> {
    let dir = &shared.canonical;
    if dir.starts_with(root) || root.starts_with(dir) {
        return Err("shared target must be outside the workspace and cannot contain it".into());
    }
    for ancestor in dir.ancestors() {
        if metadata(&ancestor.join(".git"))?.is_some() {
            return Err("shared target must be outside Git checkouts".into());
        }
    }
    check_ancestors(dir)?;
    let path = sibling_lock(dir).ok_or("shared target needs a sibling lock path")?;
    for _ in 0..LOCK_ATTEMPTS {
        if metadata(&path)?.is_some_and(|meta| !meta.file_type().is_file()) {
            return Err(format!("shared lock must be a regular file: {}", path.display()));
        }
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                eprintln!("shared target: waiting for {}", dir.display());
                file.lock().map_err(|e| format!("lock {}: {e}", path.display()))?;
            }
            Err(TryLockError::Error(e)) => return Err(format!("lock {}: {e}", path.display())),
        }
        if !same_file(&file, &path) {
            continue;
        }
        // A parent publishes its sidecar before testing emptiness. A child publishes its
        // own inside that parent before this recheck, so both cannot pass admission.
        check_ancestors(dir)?;
        return Ok(file);
    }
    Err(format!("{} keeps being replaced while waiting for it; is something cleaning the directory?", path.display()))
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
        _ => false,
    }
}

/// Rust opens files allowing deletion on Windows, so the held file can be replaced too: compare
/// creation times, which a recreated file does not share.
#[cfg(windows)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.creation_time() == named.creation_time(),
        _ => false,
    }
}

#[cfg(not(any(unix, windows)))]
fn same_file(_file: &File, path: &Path) -> bool {
    path.is_file()
}

/// Removes the workspace crates' fingerprints (and incremental state) from every profile of every
/// target in `dir`, so Cargo rebuilds them from this checkout's sources. Third-party artifacts stay.
/// Each profile's own cargo lock is held meanwhile, so a cargo still building there (from an
/// xtask that was killed) finishes first. Returns how many entries were removed.
pub fn invalidate(dir: &Path, members: &BTreeSet<String>) -> Result<usize, String> {
    let mut removed = 0usize;
    for profile in profile_dirs(dir)? {
        let _cargo = cargo_lock(&profile)?;
        let fingerprints = profile.join(".fingerprint");
        for entry in fs::read_dir(&fingerprints).map_err(|e| format!("read {}: {e}", fingerprints.display()))? {
            let entry = entry.map_err(|e| format!("read {}: {e}", fingerprints.display()))?;
            let name = entry.file_name();
            if name.to_str().and_then(package_of).is_some_and(|package| members.contains(package)) {
                remove(&entry.path())?;
                removed = removed.saturating_add(1);
            }
        }
        // Cargo keeps incremental state for workspace crates only.
        let incremental = profile.join("incremental");
        if incremental.is_dir() {
            remove(&incremental)?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

fn cargo_lock(profile: &Path) -> Result<Option<File>, String> {
    let path = profile.join(".cargo-lock");
    let Some(meta) = metadata(&path)? else { return Ok(None) };
    if !meta.file_type().is_file() {
        return Err(format!("Cargo profile lock must be a regular file: {}", path.display()));
    }
    let file = OpenOptions::new().read(true).write(true).open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            eprintln!("shared target: waiting for a cargo still building in {}", profile.display());
            file.lock().map_err(|e| format!("lock {}: {e}", path.display()))?;
        }
        Err(TryLockError::Error(e)) => return Err(format!("lock {}: {e}", path.display())),
    }
    Ok(Some(file))
}

/// `<dir>/<profile>` and `<dir>/<triple>/<profile>`: every directory holding a `.fingerprint`.
fn profile_dirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for level1 in subdirs(dir)? {
        if has_fingerprints(&level1)? {
            found.push(level1);
            continue;
        }
        for level2 in subdirs(&level1)? {
            if has_fingerprints(&level2)? {
                found.push(level2);
            }
        }
    }
    Ok(found)
}

fn has_fingerprints(profile: &Path) -> Result<bool, String> {
    let path = profile.join(".fingerprint");
    match metadata(&path)? {
        Some(meta) if meta.file_type().is_dir() => Ok(true),
        Some(_) => Err(format!("Cargo fingerprints must be a real directory: {}", path.display())),
        None => Ok(false),
    }
}

fn subdirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        // Never follow a symlink out of the build directory.
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// The package a fingerprint directory belongs to: Cargo names them `<package>-<hex hash>`.
fn package_of(entry: &str) -> Option<&str> {
    let (package, hash) = entry.rsplit_once('-')?;
    (!package.is_empty() && (8..=64).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(package)
}

/// The shared wrapper accepts Cargo built-ins and the extensions dispatched directly by xtask.
pub fn refused_argument(command: &str, args: &[&str]) -> Option<&'static str> {
    if command == "clean" {
        return Some("use `cargo xtask shared --clean` to clean an owned cache");
    }
    if !matches!(
        command,
        "build"
            | "check"
            | "test"
            | "run"
            | "bench"
            | "doc"
            | "rustc"
            | "rustdoc"
            | "fmt"
            | "clippy"
            | "nextest"
            | "metadata"
            | "tree"
            | "fetch"
            | "update"
            | "generate-lockfile"
            | "locate-project"
            | "pkgid"
            | "verify-project"
            | "--help"
            | "--version"
            | "-V"
            | "--list"
    ) {
        return Some("use an explicit workspace Cargo command. Cargo aliases and arbitrary extensions are refused");
    }
    if command == "nextest" && !matches!(args, ["run" | "list" | "archive", ..] | [] | ["--help" | "-h" | "--version" | "-V"]) {
        return Some("select nextest run, list or archive explicitly");
    }
    for arg in args.iter().take_while(|&&arg| arg != "--") {
        let flag = arg.split_once('=').map_or(*arg, |(flag, _)| flag);
        if flag.starts_with("-C") || matches!(flag, "--config" | "--target-dir" | "--build-dir" | "--manifest-path") {
            return Some("Cargo arguments cannot override the invoking workspace or shared directory");
        }
    }
    None
}

pub fn configure(command: &mut std::process::Command) {
    if ROUTE.get().is_some() || inherited() {
        // Do not let a child program's own Cargo launcher inherit the shared
        // directories. The options below apply to the intended Cargo invocation.
        command.env_remove(ENV).env_remove(INHERITED).env_remove("CARGO_TARGET_DIR").env_remove("CARGO_BUILD_TARGET_DIR").env_remove("CARGO_BUILD_BUILD_DIR");
    }
    if ROUTE.get().is_some() {
        command.env(INHERITED, "1");
    }
}

pub fn cargo_options(command: &mut std::process::Command, root: &Path) {
    let isolated;
    let value = if let Some(value) = ROUTE.get() {
        value
    } else if inherited() {
        // Without a shared override in its environment, the nested launcher can
        // still use its default isolated settings if an explicit path is invalid.
        let Ok(value) = cargo_config_value(&root.join("target")) else { return };
        isolated = value;
        &isolated
    } else {
        return;
    };
    command.args(["--config", &format!("build.target-dir={value}"), "--config", &format!("build.build-dir={value}")]);
}

fn cargo_config_value(dir: &Path) -> Result<String, String> {
    let path = dir.to_str().ok_or("shared target path must be valid UTF-8")?;
    // Cargo expands template variables in build.build-dir.
    if path.contains(['{', '}']) {
        return Err("shared target path cannot contain Cargo template braces".into());
    }
    // JSON string escapes also work in TOML basic strings, with DEL escaped explicitly.
    Ok(serde_json::Value::String(path.into()).to_string().replace('\u{7f}', "\\u007f"))
}

/// Resolve supported extensions directly so a Cargo alias cannot shadow them in shared mode.
pub fn extension(command: &str) -> Option<PathBuf> {
    ROUTE.get()?;
    if !matches!(command, "fmt" | "clippy" | "nextest") {
        return None;
    }
    let name = format!("cargo-{command}{}", std::env::consts::EXE_SUFFIX);
    let directory = if command == "nextest" {
        std::env::var_os("CARGO_HOME").map(|home| PathBuf::from(home).join("bin"))
    } else {
        std::env::var_os("CARGO").and_then(|cargo| Path::new(&cargo).parent().map(Path::to_path_buf))
    };
    Some(directory.map(|dir| dir.join(&name)).filter(|candidate| candidate.is_file()).unwrap_or_else(|| PathBuf::from(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> BTreeSet<String> {
        ["nodecraft-engine", "nodecraft-ui-egui", "xtask"].into_iter().map(String::from).collect()
    }

    fn make(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("lib-x"), "fingerprint").unwrap();
    }

    #[test]
    fn package_names_come_from_the_fingerprint_directory() {
        assert_eq!(package_of("nodecraft-engine-0123456789abcdef"), Some("nodecraft-engine"));
        assert_eq!(package_of("serde-0123456789abcdef"), Some("serde"));
        assert_eq!(package_of("nodecraft-engine"), None, "no hash");
        assert_eq!(package_of("nodecraft-engine-xyz"), None, "not hex");
        assert_eq!(package_of("-0123456789abcdef"), None);
    }

    #[test]
    fn only_workspace_fingerprints_are_removed_in_every_profile_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let keep = [
            "debug/.fingerprint/serde-0123456789abcdef",
            "debug/.fingerprint/nodecraft-engine-extra-0123456789abcdef",
            "debug/.fingerprint/nodecraft-0123456789abcdef",
            "wasm32-unknown-unknown/debug/.fingerprint/egui-fedcba9876543210",
        ];
        let remove = [
            "debug/.fingerprint/nodecraft-engine-0123456789abcdef",
            "debug/.fingerprint/nodecraft-engine-fedcba9876543210",
            "debug/.fingerprint/xtask-0123456789abcdef",
            "release/.fingerprint/nodecraft-ui-egui-0123456789abcdef",
            "wasm32-unknown-unknown/debug/.fingerprint/nodecraft-ui-egui-0123456789abcdef",
        ];
        for path in keep.iter().chain(&remove) {
            make(&root.join(path));
        }
        fs::write(root.join("debug/.fingerprint/xtask-00000000aaaaaaaa"), "a stray file").unwrap();
        fs::write(root.join("debug/.cargo-lock"), "").unwrap();
        make(&root.join("debug/incremental/nodecraft_engine-0123456789abcdef"));
        fs::create_dir_all(root.join("debug/deps")).unwrap();
        fs::create_dir_all(root.join("nextest")).unwrap();

        assert_eq!(invalidate(root, &members()).unwrap(), remove.len() + 2);
        for path in keep {
            assert!(root.join(path).is_dir(), "{path} is kept");
        }
        for path in remove {
            assert!(!root.join(path).exists(), "{path} is removed");
        }
        assert!(!root.join("debug/incremental").exists());
        assert!(root.join("debug/deps").is_dir(), "outputs are rebuilt in place, not deleted");
        assert_eq!(invalidate(root, &members()).unwrap(), 0, "a second pass finds nothing");
    }

    #[test]
    fn an_empty_directory_has_nothing_to_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(invalidate(dir.path(), &members()).unwrap(), 0);
    }

    fn shared(path: PathBuf) -> Shared {
        let config_value = cargo_config_value(&path).unwrap();
        Shared { path: path.clone(), canonical: path, config_value }
    }

    #[test]
    fn a_new_checkout_invalidates_and_the_same_checkout_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cache = root.join("cache");
        let one = root.join("checkout-one");
        let two = root.join("checkout-two");
        let fingerprint = cache.join("debug/.fingerprint/nodecraft-engine-0123456789abcdef");

        prepare(&one, &cache, &members()).unwrap();
        assert_eq!(read_stamp(&cache).unwrap().unwrap(), stamp_of(&one));

        make(&fingerprint);
        prepare(&one, &cache, &members()).unwrap();
        assert!(fingerprint.is_dir(), "the same checkout keeps its own build");

        prepare(&two, &cache, &members()).unwrap();
        assert!(!fingerprint.exists(), "another checkout rebuilds the workspace crates");
        assert_eq!(read_stamp(&cache).unwrap().unwrap(), stamp_of(&two));
    }

    #[test]
    fn an_interrupted_cache_refuses_reuse_and_clean() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cache = shared(root.join("cache"));
        let checkout = root.join("checkout");
        let holder = lock(&checkout, &cache).unwrap();
        prepare(&checkout, &cache.canonical, &members()).unwrap();
        File::create_new(cache.canonical.join(ACTIVE)).unwrap();
        let output = cache.canonical.join("debug/deps");
        make(&output);
        drop(holder); // Simulate an exiting holder that did not reach normal completion.
        assert!(prepare(&checkout, &cache.canonical, &members()).unwrap_err().contains("interrupted"));
        assert!(clean(&checkout, &cache).unwrap_err().contains("interrupted"));
        assert!(output.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_lock_file_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).unwrap();
        assert!(same_file(&file, &path));
        fs::remove_file(&path).unwrap();
        File::create(&path).unwrap();
        assert!(!same_file(&file, &path));
    }

    #[test]
    fn clean_refuses_unowned_directories_and_keeps_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let checkout = root.join("checkout");
        let cache = shared(root.join("cache"));
        let unrelated = root.join("unrelated");
        let existing = shared(unrelated.clone());
        make(&unrelated);
        assert!(clean(&checkout, &existing).unwrap_err().contains("unowned"));
        assert!(prepare(&checkout, &unrelated, &members()).unwrap_err().contains("new or empty"));
        assert!(unrelated.join("lib-x").is_file());

        prepare(&checkout, &cache.canonical, &members()).unwrap();
        make(&cache.canonical.join("debug/deps"));
        clean(&checkout, &cache).unwrap();
        assert!(!cache.canonical.exists());
        assert!(sibling_lock(&cache.canonical).unwrap().is_file());
    }

    #[test]
    fn nested_targets_cannot_be_admitted_in_either_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let checkout = root.join("checkout");
        let parent = shared(root.join("parent"));
        let child = shared(parent.canonical.join("child"));
        fs::create_dir(&parent.canonical).unwrap();

        let child_lock = lock(&checkout, &child).unwrap();
        let parent_lock = lock(&checkout, &parent).unwrap();
        assert!(prepare(&checkout, &parent.canonical, &members()).unwrap_err().contains("new or empty"));
        assert!(check_ancestors(&child.canonical).unwrap_err().contains("cannot be nested"));
        drop((parent_lock, child_lock));

        let parent = shared(root.join("first-parent"));
        let child = shared(parent.canonical.join("child"));
        prepare(&checkout, &parent.canonical, &members()).unwrap();
        drop(lock(&checkout, &parent).unwrap());
        assert!(lock(&checkout, &child).unwrap_err().contains("cannot be nested"));
        clean(&checkout, &parent).unwrap();
        assert!(check_ancestors(&child.canonical).unwrap_err().contains("cannot be nested"));
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_and_invalidation_do_not_follow_directory_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let checkout = root.join("checkout");
        let cache = shared(root.join("cache"));
        let outside = root.join("outside");
        let member = outside.join("nodecraft-engine-0123456789abcdef");
        make(&member);
        std::os::unix::fs::symlink(&outside, &cache.canonical).unwrap();
        assert!(clean(&checkout, &cache).unwrap_err().contains("real directory"));
        fs::remove_file(&cache.canonical).unwrap();

        prepare(&checkout, &cache.canonical, &members()).unwrap();
        fs::create_dir(cache.canonical.join("debug")).unwrap();
        std::os::unix::fs::symlink(&outside, cache.canonical.join("debug/.fingerprint")).unwrap();
        assert!(invalidate(&cache.canonical, &members()).unwrap_err().contains("real directory"));
        clean(&checkout, &cache).unwrap();
        assert!(member.join("lib-x").is_file());
    }

    #[test]
    fn arguments_that_escape_the_lock_or_the_invalidation_are_refused() {
        assert!(refused_argument("clean", &[]).is_some());
        assert!(refused_argument("wipe", &[]).is_some(), "an alias may expand to cargo clean");
        assert!(refused_argument("other", &[]).is_some(), "an alias may select another manifest");
        assert!(refused_argument("-v", &["clean"]).is_some());
        assert!(refused_argument("+stable", &["clean", "-p", "xtask"]).is_some());
        assert!(refused_argument("test", &["-p", "xtask", "--", "clean"]).is_none());
        assert!(refused_argument("build", &["--target-dir", "/elsewhere"]).is_some());
        assert!(refused_argument("build", &["--target-dir=/elsewhere"]).is_some());
        assert!(refused_argument("test", &["--manifest-path", "tools/eyecandy/Cargo.toml"]).is_some());
        assert!(refused_argument("test", &["--config=alias.test='clean'"]).is_some());
        assert!(refused_argument("test", &["-p", "nodecraft-engine", "--", "--target-dir"]).is_none());
        assert!(refused_argument("build", &["-p", "nodecraft"]).is_none());
    }
}
