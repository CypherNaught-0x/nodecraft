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
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The opt-in: an absolute path to the shared build directory.
pub const ENV: &str = "NODECRAFT_SHARED_TARGET";
/// Set on every cargo command xtask runs in the shared directory, to the lock holder's token. An
/// xtask started inside one (a test that runs the xtask binary, say) is covered by that lock while
/// the holder still owns it.
pub const HELD: &str = "NODECRAFT_SHARED_TARGET_HELD";
/// The checkout that last built in the directory.
const STAMP: &str = ".nodecraft-checkout";
/// Locked for the whole xtask command. xtask never deletes it.
const LOCK: &str = ".nodecraft-lock";
/// The current holder: its token on the first line, then its checkout, for the waiting message.
const OWNER: &str = ".nodecraft-owner";
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
}

/// The directory and holder token every child cargo command uses, once [`acquire`] has run.
static ROUTE: OnceLock<(PathBuf, String)> = OnceLock::new();

/// The shared directory and lock token child cargo commands carry, once [`acquire`] has run.
pub fn route() -> Option<(&'static Path, &'static str)> {
    ROUTE.get().map(|(path, token)| (path.as_path(), token.as_str()))
}

/// Holds the directory lock until dropped (the end of the xtask command).
pub struct Guard {
    lock: Option<(File, PathBuf, String)>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Withdraw the token first, so a process that inherited it can no longer skip the lock.
        if let Some((_, dir, token)) = &self.lock
            && owner_token(dir).as_deref() == Some(token.as_str())
        {
            let _ = fs::remove_file(dir.join(OWNER));
        }
    }
}

/// The configured shared directory, or `None` when the variable is unset or empty.
pub fn configured() -> Result<Option<Shared>, String> {
    let Some(value) = std::env::var_os(ENV).filter(|v| !v.is_empty()) else { return Ok(None) };
    let given = PathBuf::from(value);
    if !given.is_absolute() {
        return Err(format!("{ENV} must be an absolute path"));
    }
    fs::create_dir_all(&given).map_err(|e| format!("create {ENV} directory: {e}"))?;
    let path = std::path::absolute(&given).map_err(|e| format!("resolve {ENV} directory: {e}"))?;
    let canonical = path.canonicalize().map_err(|e| format!("resolve {ENV} directory: {e}"))?;
    let shared = Shared { path, canonical };
    if !held(&shared) {
        // xtask itself is built before it can take the lock, so it must not be built in the
        // shared directory, however that was configured.
        for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
            if std::env::var_os(var).filter(|v| !v.is_empty()).is_some_and(|own| inside(Path::new(&own), &shared.canonical)) {
                return Err(format!("set {ENV}, not {var}, to the shared directory: xtask itself must build outside it"));
            }
        }
        if std::env::current_exe().is_ok_and(|exe| inside(&exe, &shared.canonical)) {
            return Err(format!(
                "xtask was built inside {ENV}, outside its lock (check CARGO_TARGET_DIR and build.target-dir); build xtask in the worktree's own target/"
            ));
        }
    }
    Ok(Some(shared))
}

fn inside(path: &Path, dir: &Path) -> bool {
    path.canonicalize().is_ok_and(|path| path.starts_with(dir))
}

/// Whether a live ancestor xtask holds `shared`: its token is in our environment and still in the
/// owner file.
fn held(shared: &Shared) -> bool {
    let token_matches = std::env::var(HELD).is_ok_and(|token| !token.is_empty() && owner_token(&shared.path).as_deref() == Some(token.as_str()));
    // A holder that was killed leaves its token behind: the lock must also really be held now.
    token_matches && lock_is_held(&shared.path)
}

fn lock_is_held(dir: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(dir.join(LOCK)) else { return false };
    // Taking it here means nobody holds it; dropping the file releases it again at once.
    matches!(file.try_lock(), Err(TryLockError::WouldBlock))
}

fn owner_token(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(OWNER)).ok()?.lines().next().map(str::to_owned).filter(|t| !t.is_empty())
}

/// Locks the directory, invalidates another checkout's workspace artifacts, and routes every later
/// [`crate::cargo`] command into it.
pub fn acquire(root: &Path, shared: &Shared, members: &BTreeSet<String>) -> Result<Guard, String> {
    let (guard, token) = if held(shared) {
        // The ancestor's lock covers this command, but only for the checkout it prepared.
        if read_stamp(&shared.path).as_deref() != Some(stamp_of(root).as_str()) {
            return Err(format!("{} is held for another checkout by a parent xtask; run this one outside it", shared.path.display()));
        }
        (Guard { lock: None }, std::env::var(HELD).unwrap_or_default())
    } else {
        let guard = prepare(root, &shared.path, members)?;
        let token = guard.lock.as_ref().map(|(_, _, token)| token.clone()).unwrap_or_default();
        (guard, token)
    };
    eprintln!("shared target: {}", shared.path.display());
    // A second acquire in one process would be a bug; the first route stays in force.
    let _ = ROUTE.set((shared.path.clone(), token));
    Ok(guard)
}

fn stamp_of(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn read_stamp(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(STAMP)).ok()
}

/// Locks `dir` and, when another checkout built there last, invalidates its workspace artifacts.
fn prepare(root: &Path, dir: &Path, members: &BTreeSet<String>) -> Result<Guard, String> {
    let guard = lock(root, dir)?;
    let checkout = stamp_of(root);
    let last = read_stamp(dir);
    if last.as_deref() != Some(checkout.as_str()) {
        let removed = invalidate(dir, members)?;
        if removed > 0 {
            eprintln!(
                "shared target: last built by {}, rebuilding the workspace crates ({removed} entries cleared)",
                last.as_deref().unwrap_or("an unknown checkout")
            );
        }
        // Only after a complete invalidation: a failed one is retried next time.
        fs::write(dir.join(STAMP), &checkout).map_err(|e| format!("write shared target stamp: {e}"))?;
    }
    Ok(guard)
}

/// Deletes everything in the directory except the lock, holding the lock.
pub fn clean(root: &Path, shared: &Shared) -> Result<(), String> {
    if held(shared) {
        return Err(format!("{} is in use by a parent xtask; clean it from outside", shared.path.display()));
    }
    let dir = &shared.path;
    let _guard = lock(root, dir)?;
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        if entry.file_name() == LOCK || entry.file_name() == OWNER {
            continue;
        }
        remove(&entry.path())?;
    }
    println!("shared target: cleaned {}", dir.display());
    Ok(())
}

fn remove(path: &Path) -> Result<(), String> {
    let kind = fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    let result = if kind.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    result.map_err(|e| format!("remove {}: {e}", path.display()))
}

fn lock(root: &Path, dir: &Path) -> Result<Guard, String> {
    let path = dir.join(LOCK);
    for _ in 0..LOCK_ATTEMPTS {
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let owner = fs::read_to_string(dir.join(OWNER)).unwrap_or_default();
                let owner = owner.lines().nth(1).unwrap_or("another checkout").to_owned();
                eprintln!("shared target: waiting for {} (in use by {owner})", dir.display());
                file.lock().map_err(|e| format!("lock {}: {e}", path.display()))?;
            }
            Err(TryLockError::Error(e)) => return Err(format!("lock {}: {e}", path.display())),
        }
        // Something outside xtask may have deleted the lock file while we waited; then another
        // process can hold a new one, and this lock protects nothing. Try again.
        if !same_file(&file, &path) {
            continue;
        }
        let token = token()?;
        let tmp = dir.join(format!("{OWNER}.{}", std::process::id()));
        let owner = format!("{token}\n{} (pid {})\n", root.display(), std::process::id());
        fs::write(&tmp, &owner).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        if fs::rename(&tmp, dir.join(OWNER)).is_err() {
            // Windows can refuse the rename while something else has the file open; only the lock
            // holder writes it, so a direct write is safe too.
            let _ = fs::remove_file(&tmp);
            fs::write(dir.join(OWNER), &owner).map_err(|e| format!("write {}: {e}", dir.join(OWNER).display()))?;
        }
        return Ok(Guard { lock: Some((file, dir.to_path_buf(), token)) });
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

fn token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("make a lock token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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
    if !path.is_file() {
        return Ok(None);
    }
    let file = OpenOptions::new().read(true).write(true).open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    if let Err(TryLockError::WouldBlock) = file.try_lock() {
        eprintln!("shared target: waiting for a cargo still building in {}", profile.display());
        file.lock().map_err(|e| format!("lock {}: {e}", path.display()))?;
    }
    Ok(Some(file))
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

/// Cargo arguments `cargo xtask shared --` refuses: they would skip the lock or the invalidation.
pub fn refused_argument(args: &[&str]) -> Option<&'static str> {
    for arg in args {
        if *arg == "--" {
            break;
        }
        // Anywhere before `--`: options such as `-v` or `+toolchain` may come first.
        if *arg == "clean" {
            return Some("`cargo clean` would delete the lock; use `cargo xtask shared --clean`");
        }
        if *arg == "--target-dir" || arg.starts_with("--target-dir=") {
            return Some("`--target-dir` would build outside the shared directory");
        }
        if *arg == "--manifest-path" || arg.starts_with("--manifest-path=") {
            return Some("`--manifest-path` can build another workspace, whose crates are not invalidated; run it from that workspace");
        }
    }
    None
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
        assert_eq!(read_stamp(shared).unwrap(), stamp_of(&one));

        make(&fingerprint);
        drop(prepare(&one, shared, &members()).unwrap());
        assert!(fingerprint.is_dir(), "the same checkout keeps its own build");

        drop(prepare(&two, shared, &members()).unwrap());
        assert!(!fingerprint.exists(), "another checkout rebuilds the workspace crates");
        assert_eq!(read_stamp(shared).unwrap(), stamp_of(&two));
    }

    #[test]
    fn the_holder_token_is_withdrawn_when_the_lock_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let guard = lock(Path::new("/checkout"), dir.path()).unwrap();
        let token = owner_token(dir.path()).unwrap();
        assert_eq!(token.len(), 32);
        assert_eq!(guard.lock.as_ref().map(|(_, _, t)| t.as_str()), Some(token.as_str()));
        let shared = Shared { path: dir.path().to_path_buf(), canonical: dir.path().canonicalize().unwrap() };
        assert!(lock_is_held(&shared.path), "a second handle sees the live lock");
        drop(guard);
        assert!(!lock_is_held(&shared.path), "released with its holder");
        assert_eq!(owner_token(dir.path()), None, "an inherited token no longer skips the lock");
        let again = lock(Path::new("/checkout"), dir.path()).unwrap();
        assert_ne!(owner_token(dir.path()), Some(token), "every holder has its own token");
        drop(again);
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_lock_file_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK);
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).unwrap();
        assert!(same_file(&file, &path));
        fs::remove_file(&path).unwrap();
        File::create(&path).unwrap();
        assert!(!same_file(&file, &path));
    }

    #[test]
    fn clean_keeps_only_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared { path: dir.path().to_path_buf(), canonical: dir.path().canonicalize().unwrap() };
        make(&shared.path.join("debug/deps"));
        fs::write(shared.path.join(STAMP), "somewhere").unwrap();
        clean(Path::new("/checkout"), &shared).unwrap();
        let left: Vec<_> = fs::read_dir(&shared.path).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from(LOCK)]);
    }

    #[test]
    fn arguments_that_escape_the_lock_or_the_invalidation_are_refused() {
        assert!(refused_argument(&["clean"]).is_some());
        assert!(refused_argument(&["-v", "clean"]).is_some());
        assert!(refused_argument(&["+stable", "clean", "-p", "xtask"]).is_some());
        assert!(refused_argument(&["test", "-p", "xtask", "--", "clean"]).is_none(), "a test filter named clean passes");
        assert!(refused_argument(&["build", "--target-dir", "/elsewhere"]).is_some());
        assert!(refused_argument(&["build", "--target-dir=/elsewhere"]).is_some());
        assert!(refused_argument(&["test", "--manifest-path", "tools/eyecandy/Cargo.toml"]).is_some());
        assert!(refused_argument(&["test", "-p", "nodecraft-engine", "--", "--target-dir"]).is_none(), "test binary arguments pass");
        assert!(refused_argument(&["build", "-p", "nodecraft"]).is_none());
    }
}
