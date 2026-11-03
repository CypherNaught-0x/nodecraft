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
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The opt-in: an absolute path to the shared build directory.
pub const ENV: &str = "NODECRAFT_SHARED_TARGET";
/// The checkout that last built in the directory.
const STAMP: &str = ".nodecraft-checkout";
/// Locked for the whole xtask command. Never deleted, so a waiter keeps a valid handle.
const LOCK: &str = ".nodecraft-lock";
/// Who holds the lock, for the waiting message. Best effort.
const OWNER: &str = ".nodecraft-owner";
/// Set on every cargo command xtask runs in the shared directory: an xtask started inside one
/// (a test that runs the xtask binary, say) is covered by its ancestor's lock and must not wait
/// for it.
pub const HELD: &str = "NODECRAFT_SHARED_TARGET_HELD";

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// The shared directory every child cargo command builds in, once [`acquire`] has run.
pub fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

/// Holds the directory lock until dropped (the end of the xtask command).
pub struct Guard {
    _lock: Option<File>,
}

/// Whether an ancestor xtask already holds `dir`.
fn held(dir: &Path) -> bool {
    std::env::var_os(HELD).is_some_and(|held| Path::new(&held) == dir)
}

/// The configured shared directory, or `None` when the variable is unset or empty.
pub fn configured() -> Result<Option<PathBuf>, String> {
    let Some(value) = std::env::var_os(ENV).filter(|v| !v.is_empty()) else { return Ok(None) };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{ENV} must be an absolute path"));
    }
    fs::create_dir_all(&path).map_err(|e| format!("create {ENV} directory: {e}"))?;
    let path = path.canonicalize().map_err(|e| format!("resolve {ENV} directory: {e}"))?;
    // xtask itself is built in CARGO_TARGET_DIR before any of this runs, outside the lock, so it
    // must not be the shared directory.
    // Inside a held directory, CARGO_TARGET_DIR is the shared one by design.
    if !held(&path)
        && let Some(own) = std::env::var_os("CARGO_TARGET_DIR").filter(|v| !v.is_empty())
        && Path::new(&own).canonicalize().is_ok_and(|own| own == path)
    {
        return Err(format!("set {ENV}, not CARGO_TARGET_DIR, to the shared directory: xtask itself must build outside it"));
    }
    Ok(Some(path))
}

/// Locks `dir`, invalidates another checkout's workspace artifacts, and routes every later
/// [`crate::cargo`] command into it.
pub fn acquire(root: &Path, dir: &Path, members: &BTreeSet<String>) -> Result<Guard, String> {
    let guard = if held(dir) {
        // The ancestor's lock covers this command, but only for the checkout it prepared.
        let last = fs::read_to_string(dir.join(STAMP)).unwrap_or_default();
        if last != root.display().to_string() {
            return Err(format!("{} is held for another checkout by a parent xtask; run this one outside it", dir.display()));
        }
        Guard { _lock: None }
    } else {
        prepare(root, dir, members)?
    };
    eprintln!("shared target: {}", dir.display());
    // A second acquire in one process would be a bug; the first directory stays in force.
    let _ = DIR.set(dir.to_path_buf());
    Ok(guard)
}

/// Locks `dir` and, when another checkout built there last, invalidates its workspace artifacts.
fn prepare(root: &Path, dir: &Path, members: &BTreeSet<String>) -> Result<Guard, String> {
    let lock = lock(root, dir)?;
    let checkout = root.display().to_string();
    let last = fs::read_to_string(dir.join(STAMP)).ok();
    if last.as_deref() != Some(checkout.as_str()) {
        let removed = invalidate(dir, members)?;
        if removed > 0 {
            eprintln!(
                "shared target: last built by {}, rebuilding the workspace crates ({removed} entries cleared)",
                last.as_deref().unwrap_or("an unknown checkout")
            );
        }
        fs::write(dir.join(STAMP), &checkout).map_err(|e| format!("write shared target stamp: {e}"))?;
    }
    Ok(Guard { _lock: Some(lock) })
}

/// Deletes everything in `dir` except the lock, holding the lock.
pub fn clean(root: &Path, dir: &Path) -> Result<(), String> {
    if held(dir) {
        return Err(format!("{} is in use by a parent xtask; clean it from outside", dir.display()));
    }
    let _lock = lock(root, dir)?;
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        if entry.file_name() == LOCK {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| format!("inspect {}: {e}", path.display()))?;
        let result = if kind.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        result.map_err(|e| format!("remove {}: {e}", path.display()))?;
    }
    println!("shared target: cleaned {}", dir.display());
    Ok(())
}

fn lock(root: &Path, dir: &Path) -> Result<File, String> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let owner = fs::read_to_string(dir.join(OWNER)).unwrap_or_default();
            let owner = owner.trim();
            eprintln!("shared target: waiting for {} (in use by {})", dir.display(), if owner.is_empty() { "another checkout" } else { owner });
            file.lock().map_err(|e| format!("lock {}: {e}", path.display()))?;
        }
        Err(TryLockError::Error(e)) => return Err(format!("lock {}: {e}", path.display())),
    }
    if let Ok(mut owner) = File::create(dir.join(OWNER)) {
        let _ = write!(owner, "{} (pid {})", root.display(), std::process::id());
    }
    Ok(file)
}

/// Removes the workspace crates' fingerprints (and incremental state) from every profile of every
/// target in `dir`, so Cargo rebuilds them from this checkout's sources. Third-party artifacts stay.
/// Returns how many entries were removed.
pub fn invalidate(dir: &Path, members: &BTreeSet<String>) -> Result<usize, String> {
    let mut removed = 0usize;
    for profile in profile_dirs(dir)? {
        let fingerprints = profile.join(".fingerprint");
        for entry in fs::read_dir(&fingerprints).map_err(|e| format!("read {}: {e}", fingerprints.display()))? {
            let entry = entry.map_err(|e| format!("read {}: {e}", fingerprints.display()))?;
            let name = entry.file_name();
            if name.to_str().and_then(package_of).is_some_and(|package| members.contains(package)) {
                let path = entry.path();
                fs::remove_dir_all(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
                removed = removed.saturating_add(1);
            }
        }
        // Cargo keeps incremental state for workspace crates only.
        let incremental = profile.join("incremental");
        if incremental.is_dir() {
            fs::remove_dir_all(&incremental).map_err(|e| format!("remove {}: {e}", incremental.display()))?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

/// `<dir>/<profile>` and `<dir>/<triple>/<profile>`: every directory holding a `.fingerprint`.
fn profile_dirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for level1 in subdirs(dir)? {
        if level1.join(".fingerprint").is_dir() {
            found.push(level1);
            continue;
        }
        for level2 in subdirs(&level1)? {
            if level2.join(".fingerprint").is_dir() {
                found.push(level2);
            }
        }
    }
    Ok(found)
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
        make(&root.join("debug/incremental/nodecraft_engine-0123456789abcdef"));
        fs::create_dir_all(root.join("debug/deps")).unwrap();
        fs::create_dir_all(root.join("nextest")).unwrap();

        assert_eq!(invalidate(root, &members()).unwrap(), remove.len() + 1);
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

    #[test]
    fn a_new_checkout_invalidates_and_the_same_checkout_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path();
        let one = shared.join("checkout-one");
        let two = shared.join("checkout-two");
        let fingerprint = shared.join("debug/.fingerprint/nodecraft-engine-0123456789abcdef");

        make(&fingerprint);
        drop(prepare(&one, shared, &members()).unwrap());
        assert!(!fingerprint.exists(), "an unstamped directory may hold another checkout's build");
        assert_eq!(fs::read_to_string(shared.join(STAMP)).unwrap(), one.display().to_string());

        make(&fingerprint);
        drop(prepare(&one, shared, &members()).unwrap());
        assert!(fingerprint.is_dir(), "the same checkout keeps its own build");

        drop(prepare(&two, shared, &members()).unwrap());
        assert!(!fingerprint.exists(), "another checkout rebuilds the workspace crates");
        assert_eq!(fs::read_to_string(shared.join(STAMP)).unwrap(), two.display().to_string());
    }

    #[test]
    fn clean_keeps_only_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path();
        make(&shared.join("debug/deps"));
        fs::write(shared.join(STAMP), "somewhere").unwrap();
        clean(Path::new("/checkout"), shared).unwrap();
        let left: Vec<_> = fs::read_dir(shared).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from(LOCK)]);
    }
}
