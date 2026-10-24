//! Staging and swapping in an update (#540) once `update_install::download` has verified it.
//!
//! How this copy is replaced depends on how it was installed ([`Kind`]):
//!
//! - **macOS app bundle:** the zipped bundle is unpacked beside the running one
//!   (`.NodeCraft-update-<v>.app`, same volume, so the swap is two renames), its code signature
//!   checked, and on restart the running bundle is renamed aside and the new one put in its place.
//! - **Linux AppImage:** the new AppImage is written beside the running one and renamed over it
//!   on restart; the running process keeps its open file.
//! - **Windows MSI install:** Windows can't replace a running executable, so on restart a copy
//!   of this executable runs as a helper (`--finish-update`): it waits for the app to exit, runs
//!   the MSI (whose `MajorUpgrade` replaces the old version), then starts NodeCraft again.
//!
//! The immediate relaunch (or Windows helper) gets `--wait-for <pid>` so it opens the project only
//! after this copy closes it. The helper then starts the installed app, forwarding `--control` and
//! its allowed folders only while they remain valid. The outcome is written to `last-update.json`
//! in the updates folder, and the next start says so.
//!
//! A staged update can wait days for the restart, in a folder the person can write to, so it is
//! checked again just before it is used (#567): the bundle's code signature, or the AppImage's or
//! MSI's BLAKE3 against the signed feed's (the Windows helper checks the MSI once more, holding
//! it open so nothing can change it, before `msiexec` runs it).
//!
//! Leftovers (the renamed old bundle, a half-staged update, the helper) are removed at the next
//! start, only when no other copy of NodeCraft is running: each running copy holds a shared lock
//! on `instances.lock` in the updates folder ([`Instance`]), since another copy's staged update
//! or the old bundle it still runs from look the same. Only names this module makes are ever
//! removed.
use nodecraft_engine::build_info::VERSION;
use nodecraft_engine::update_install::Downloaded;
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bounds on the zipped app bundle.
const MAX_ENTRIES: usize = 10_000;
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// How long a new process waits for the old one to exit.
const WAIT_FOR_EXIT: Duration = Duration::from_secs(60);
/// The record of the last update, in the updates folder.
const LAST_UPDATE: &str = "last-update.json";
/// Held shared by every running copy, in the updates folder.
const INSTANCES: &str = "instances.lock";

const BUNDLE_UPDATE_PREFIX: &str = ".NodeCraft-update-";
const BUNDLE_OLD_PREFIX: &str = ".NodeCraft-old-";
const APPIMAGE_UPDATE_INFIX: &str = ".update-";
const HELPER: &str = "nodecraft-update-helper.exe";

/// How this copy of NodeCraft is installed, and so how it is replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A `.app` bundle; `verify` runs `codesign --verify` on the new one (off in tests).
    MacBundle {
        bundle: PathBuf,
        verify: bool,
    },
    AppImage {
        file: PathBuf,
    },
    /// An MSI install; `exe` is the running executable.
    WindowsMsi {
        exe: PathBuf,
    },
}

/// How this copy is installed, or why it can't update itself.
pub(crate) fn detect() -> Result<Kind, String> {
    let exe = std::env::current_exe().map_err(|e| format!("NodeCraft can't tell where it is installed ({e})"))?;
    if cfg!(target_os = "macos") {
        let bundle = exe.parent().and_then(Path::parent).and_then(Path::parent).filter(|b| b.extension().is_some_and(|e| e == "app"));
        let Some(bundle) = bundle else {
            return Err("this copy isn't an installed app".into());
        };
        let text = bundle.to_string_lossy();
        // A quarantined app opened where it was downloaded runs from a read-only random path.
        if text.starts_with("/Volumes/") || text.contains("/AppTranslocation/") {
            return Err("NodeCraft is running from the disk image or Downloads; move it to Applications first".into());
        }
        return Ok(Kind::MacBundle { bundle: bundle.to_path_buf(), verify: true });
    }
    if cfg!(target_os = "linux") {
        return appimage(std::env::var_os("APPIMAGE").map(PathBuf::from), std::env::var_os("APPDIR").map(PathBuf::from), &exe);
    }
    if cfg!(windows) {
        let dir = exe.parent().ok_or("this copy isn't an installed app")?;
        return match msi_install_dir() {
            Some(installed) if same_dir(&installed, dir) => Ok(Kind::WindowsMsi { exe }),
            _ => Err("only the installer (MSI) copy updates itself; download the new portable zip from the releases page".into()),
        };
    }
    Err("NodeCraft can't update itself on this platform".into())
}

/// The AppImage this copy runs from. `APPIMAGE` alone isn't enough: a terminal or editor that is
/// itself an AppImage passes it on to everything it starts, so the running executable must be
/// inside `APPDIR`, the image's own mount.
fn appimage(file: Option<PathBuf>, appdir: Option<PathBuf>, exe: &Path) -> Result<Kind, String> {
    let ours = appdir.is_some_and(|dir| dir.is_absolute() && exe.starts_with(&dir));
    match file {
        Some(file) if ours && file.is_absolute() && file.is_file() => Ok(Kind::AppImage { file }),
        _ => Err("only the AppImage updates itself; update this copy the way it was installed".into()),
    }
}

/// Where the MSI installed NodeCraft (packaging/windows/nodecraft.wxs records it).
fn msi_install_dir() -> Option<PathBuf> {
    let out = system("reg").args(["query", r"HKLM\Software\NodeCraft", "/v", "InstallDir"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    reg_value(&text).map(PathBuf::from)
}

/// A Windows system tool by its full path, without a console window: `Command::new` would look in
/// the running executable's folder first, and the helper runs from the user-writable updates
/// folder.
#[cfg(windows)]
fn system(name: &str) -> std::process::Command {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let mut command = std::process::Command::new(root.join("System32").join(format!("{name}.exe")));
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
    command
}

#[cfg(not(windows))]
fn system(name: &str) -> std::process::Command {
    std::process::Command::new(name)
}

/// The value in `reg query`'s answer: `    InstallDir    REG_SZ    C:\Program Files\NodeCraft\`.
fn reg_value(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.trim_start().starts_with("InstallDir"))?;
    let (_, value) = line.split_once("REG_SZ")?;
    Some(value.trim().to_owned()).filter(|value| !value.is_empty())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

/// An update unpacked and checked, waiting for the restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Staged {
    pub(crate) version: String,
    pub(crate) kind: Kind,
    /// The new bundle, AppImage or MSI.
    pub(crate) path: PathBuf,
    /// The signed feed's BLAKE3 of the payload; the AppImage and MSI are checked against it.
    pub(crate) blake3: String,
    /// The payload's size, which a check reads no further than.
    pub(crate) size: u64,
    /// The bundle's code directory hash when it was staged and verified; a bundle re-signed
    /// since (even validly, ad hoc) has another.
    pub(crate) cdhash: Option<String>,
}

/// What to start once this process has closed the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Relaunch {
    Bundle(PathBuf),
    Exec(PathBuf),
    /// The helper copy, the MSI, its BLAKE3 and the executable it starts afterwards.
    Helper {
        helper: PathBuf,
        msi: PathBuf,
        blake3: String,
        exe: PathBuf,
    },
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> Error {
    let kind = if e.kind() == std::io::ErrorKind::PermissionDenied { ErrorKind::Unsupported } else { ErrorKind::Internal };
    let hint = if kind == ErrorKind::Unsupported { "; NodeCraft can't write there, so download the update from the releases page" } else { "" };
    Error::new(kind, format!("the update couldn't be installed: {what} {}: {e}{hint}", path.display()))
}

/// Unpacks or writes `downloaded` where [`apply`] will swap it in. `updates` is NodeCraft's
/// updates folder, for the MSI.
pub(crate) fn stage(kind: &Kind, downloaded: &Downloaded, updates: &Path) -> Result<Staged, Error> {
    let version = &downloaded.version;
    let mut cdhash_staged = None;
    let path = match kind {
        Kind::MacBundle { bundle, verify } => {
            let parent = bundle.parent().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the app has no folder to update in"))?;
            let staged = parent.join(format!("{BUNDLE_UPDATE_PREFIX}{version}.app"));
            remove_ours(&staged);
            if let Err(error) = unzip_bundle(&downloaded.bytes, &staged) {
                remove_ours(&staged);
                return Err(error);
            }
            let cdhash = if *verify { verify_signature(&staged).and_then(|()| code_directory_hash(&staged)).map(Some) } else { Ok(None) };
            match cdhash {
                Ok(found) => cdhash_staged = found,
                Err(error) => {
                    remove_ours(&staged);
                    return Err(error);
                }
            }
            staged
        }
        Kind::AppImage { file } => {
            if !downloaded.bytes.starts_with(b"\x7fELF") {
                return Err(Error::new(ErrorKind::InvalidInput, "the update isn't an AppImage; nothing was installed"));
            }
            let staged = appimage_staging(file, version)?;
            write_new(&staged, &downloaded.bytes, 0o755)?;
            staged
        }
        Kind::WindowsMsi { .. } => {
            // Every MSI is an OLE compound file.
            if !downloaded.bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
                return Err(Error::new(ErrorKind::InvalidInput, "the update isn't an installer; nothing was installed"));
            }
            std::fs::create_dir_all(updates).map_err(|e| io("creating", updates, &e))?;
            let staged = updates.join(format!("nodecraft-{version}.msi"));
            write_new(&staged, &downloaded.bytes, 0o644)?;
            staged
        }
    };
    let size = u64::try_from(downloaded.bytes.len()).unwrap_or(u64::MAX);
    Ok(Staged { version: version.clone(), kind: kind.clone(), path, blake3: downloaded.blake3.clone(), size, cdhash: cdhash_staged })
}

fn appimage_staging(file: &Path, version: &str) -> Result<PathBuf, Error> {
    let name = file.file_name().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the AppImage has no file name"))?.to_string_lossy();
    let parent = file.parent().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the AppImage has no folder"))?;
    Ok(parent.join(format!(".{name}{APPIMAGE_UPDATE_INFIX}{version}")))
}

/// Writes `bytes` to a fresh `path` and flushes them to disk.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path).map_err(|e| io("writing", path, &e))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(io("writing", path, &e));
    }
    Ok(())
}

/// Unpacks a zipped `X.app` (as `ditto -c -k --keepParent` makes it) into `into`, which must not
/// exist. Only plain files and folders inside the one bundle are accepted.
pub(crate) fn unzip_bundle(bytes: &[u8], into: &Path) -> Result<(), Error> {
    let bad = |why: &str| Error::new(ErrorKind::InvalidInput, format!("the update's app bundle was refused: {why}; nothing was installed"));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| bad("it isn't a zip archive"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(bad("it has too many files"));
    }
    std::fs::create_dir(into).map_err(|e| io("creating", into, &e))?;
    let mut root: Option<std::ffi::OsString> = None;
    let mut unpacked: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| bad("an entry can't be read"))?;
        let name = entry.enclosed_name().ok_or_else(|| bad("an entry points outside it"))?;
        let mut parts = name.components();
        let Some(std::path::Component::Normal(top)) = parts.next() else {
            return Err(bad("an entry has an unusable name"));
        };
        // ditto adds macOS metadata beside the bundle; it isn't part of the app.
        if top == "__MACOSX" {
            continue;
        }
        // Extended attributes zipped as `._name` files would break the bundle's signature.
        if name.components().any(|part| part.as_os_str().to_string_lossy().starts_with("._")) {
            return Err(bad("it holds macOS metadata files (._*)"));
        }
        if !Path::new(top).extension().is_some_and(|e| e == "app") || root.as_deref().is_some_and(|r| r != top) {
            return Err(bad("it holds something other than one app bundle"));
        }
        root.get_or_insert_with(|| top.to_os_string());
        let inner: PathBuf = parts.collect();
        if entry.is_symlink() {
            return Err(bad("it holds a symbolic link"));
        }
        let target = into.join(&inner);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| io("creating", &target, &e))?;
            continue;
        }
        unpacked = unpacked.saturating_add(entry.size());
        if unpacked > MAX_UNPACKED_BYTES {
            return Err(bad("it unpacks too large"));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io("creating", parent, &e))?;
        }
        let mode = entry.unix_mode().map_or(0o644, |mode| mode & 0o755);
        let mut contents = Vec::new();
        // The header's size is a claim: read at most that much and one byte more.
        let size = entry.size();
        (&mut entry).take(size.saturating_add(1)).read_to_end(&mut contents).map_err(|_| bad("an entry can't be unpacked"))?;
        if u64::try_from(contents.len()).ok() != Some(size) {
            return Err(bad("an entry isn't the size it says"));
        }
        write_new(&target, &contents, mode)?;
    }
    if root.is_none() || !into.join("Contents").join("Info.plist").is_file() || !into.join("Contents").join("MacOS").is_dir() {
        return Err(bad("it isn't a complete app bundle"));
    }
    Ok(())
}

/// `codesign --verify`: the bundle is intact and signed (ad hoc or with a Developer ID).
fn verify_signature(bundle: &Path) -> Result<(), Error> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle)
        .output()
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("the update's signature couldn't be checked: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(ErrorKind::InvalidInput, "the update's app bundle has a broken code signature; nothing was installed"));
    }
    Ok(())
}

/// The bundle's code directory hash, as `codesign -d` reports it.
fn code_directory_hash(bundle: &Path) -> Result<String, Error> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "--verbose=4"])
        .arg(bundle)
        .output()
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("the update's signature couldn't be checked: {e}")))?;
    // codesign writes the details to stderr.
    let details = String::from_utf8_lossy(&out.stderr);
    let found = details
        .lines()
        .find_map(|line| line.strip_prefix("CDHash="))
        .map(str::trim)
        .filter(|hash| !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()));
    match found {
        Some(hash) if out.status.success() => Ok(hash.to_ascii_lowercase()),
        _ => Err(Error::new(ErrorKind::InvalidInput, "the update's app bundle has a broken code signature; nothing was installed")),
    }
}

/// Removes `path` when it is a file or folder this module made.
fn remove_ours(path: &Path) {
    let ours = path.file_name().map(|n| n.to_string_lossy().into_owned()).is_some_and(|name| {
        name.starts_with(BUNDLE_UPDATE_PREFIX)
            || name.starts_with(BUNDLE_OLD_PREFIX)
            || name.contains(APPIMAGE_UPDATE_INFIX)
            || name == HELPER
            || name.ends_with(".msi")
    });
    if !ours {
        return;
    }
    let removed = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    if let Err(e) = removed
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::info!("update leftover {} not removed: {e}", path.display());
    }
}

/// Refuses a staged file that is no longer the one the feed signed; a changed one is removed.
fn check_staged(path: &Path, blake3: &str, size: u64) -> Result<(), Error> {
    let checked = std::fs::File::open(path)
        .map_err(|e| io("checking", path, &e))
        .and_then(|mut file| nodecraft_engine::update_install::verify_staged(&mut file, blake3, size));
    if checked.is_err() {
        remove_ours(path);
    }
    checked
}

/// Swaps the staged update in, once it is checked again. `updates` holds the Windows helper. On
/// an error nothing has changed: the running copy is where it was.
pub(crate) fn apply(staged: &Staged, updates: &Path) -> Result<Relaunch, Error> {
    let relaunch = match &staged.kind {
        Kind::MacBundle { bundle, verify } => {
            if *verify {
                let same = verify_signature(&staged.path).and_then(|()| code_directory_hash(&staged.path)).and_then(|now| {
                    if staged.cdhash.as_deref() == Some(now.as_str()) {
                        Ok(())
                    } else {
                        Err(Error::new(
                            ErrorKind::InvalidInput,
                            "the staged update changed after it was checked; nothing was installed, so install the update again",
                        ))
                    }
                });
                if let Err(error) = same {
                    remove_ours(&staged.path);
                    return Err(error);
                }
            }
            let parent = bundle.parent().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the app has no folder"))?;
            let old = parent.join(format!("{BUNDLE_OLD_PREFIX}{VERSION}.app"));
            remove_ours(&old);
            std::fs::rename(bundle, &old).map_err(|e| io("moving aside", bundle, &e))?;
            if let Err(e) = std::fs::rename(&staged.path, bundle) {
                // Put the running copy back before saying anything.
                if let Err(back) = std::fs::rename(&old, bundle) {
                    log::error!("NodeCraft couldn't move {} back to {}: {back}", old.display(), bundle.display());
                    let message =
                        format!("the update couldn't be moved in ({e}), and NodeCraft is now at {}; rename it back to {}", old.display(), bundle.display());
                    return Err(Error::new(ErrorKind::Internal, message));
                }
                return Err(io("moving in", bundle, &e));
            }
            Relaunch::Bundle(bundle.clone())
        }
        Kind::AppImage { file } => {
            check_staged(&staged.path, &staged.blake3, staged.size)?;
            std::fs::rename(&staged.path, file).map_err(|e| io("replacing", file, &e))?;
            Relaunch::Exec(file.clone())
        }
        Kind::WindowsMsi { exe } => {
            check_staged(&staged.path, &staged.blake3, staged.size)?;
            let helper = updates.join(HELPER);
            remove_ours(&helper);
            std::fs::copy(exe, &helper).map_err(|e| io("copying", exe, &e))?;
            Relaunch::Helper { helper, msi: staged.path.clone(), blake3: staged.blake3.clone(), exe: exe.clone() }
        }
    };
    Ok(relaunch)
}

/// Snapshot of the startup-validated, canonical grants. Original symlink spellings must not
/// silently grant a different folder if a link changes while an update is staged.
static UPDATE_ROOTS: std::sync::OnceLock<(Vec<PathBuf>, Vec<PathBuf>)> = std::sync::OnceLock::new();

/// Called after startup has validated the control folders. A relaunch reuses only these grants.
pub fn remember_update_roots(control: bool, read: &[PathBuf], write: &[PathBuf]) {
    let _ = UPDATE_ROOTS.set(if control { (read.to_vec(), write.to_vec()) } else { (Vec::new(), Vec::new()) });
}

/// Starts the new copy (or the Windows helper); it waits for this process to exit.
pub(crate) fn relaunch(relaunch: &Relaunch, project: Option<&Path>, control: bool) -> Result<(), Error> {
    let (read, write) = UPDATE_ROOTS.get().map_or((&[][..], &[][..]), |(read, write)| (read.as_slice(), write.as_slice()));
    if control {
        check_relaunch_roots(read, write)?;
    }
    command(relaunch, project, control, read, write, &std::process::id().to_string())
        .spawn()
        .map(drop)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("NodeCraft couldn't start again after updating: {e}; open it yourself")))
}

/// Refuse to carry a grant to a different folder if a canonical path was replaced by a link,
/// or if its folder disappeared. The new app validates existence again on startup.
fn check_relaunch_roots(read: &[PathBuf], write: &[PathBuf]) -> Result<(), Error> {
    for root in read.iter().chain(write) {
        if std::fs::canonicalize(root).ok().as_deref() != Some(root.as_path()) || !root.is_dir() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("allowed folder {} changed or disappeared; restart NodeCraft manually with the folders you want to allow", root.display()),
            ));
        }
    }
    Ok(())
}

/// Append folder grants only with control mode. The final Windows launch uses this too.
fn append_grants(command: &mut std::process::Command, control: bool, read: &[PathBuf], write: &[PathBuf]) {
    if control {
        command.arg("--control");
        for root in write {
            command.arg("--root").arg(root);
        }
        for root in read {
            command.arg("--read-root").arg(root);
        }
    }
}

/// The command [`relaunch`] starts, for this process's `pid`.
fn command(relaunch: &Relaunch, project: Option<&Path>, control: bool, read: &[PathBuf], write: &[PathBuf], pid: &str) -> std::process::Command {
    let mut command = match relaunch {
        Relaunch::Bundle(bundle) => {
            let mut command = std::process::Command::new("/usr/bin/open");
            command.arg("-n").arg(bundle).arg("--args");
            command
        }
        Relaunch::Exec(file) => std::process::Command::new(file),
        Relaunch::Helper { helper, msi, blake3, exe } => {
            let mut command = std::process::Command::new(helper);
            command.arg("--finish-update").arg(msi).arg(blake3).arg(exe);
            command
        }
    };
    command.args(["--wait-for", pid]);
    append_grants(&mut command, control, read, write);
    if let Some(project) = project {
        command.arg(project);
    }
    command
}

/// Whether this process was started with `--control`, which a relaunch keeps.
pub(crate) fn started_with_control() -> bool {
    std::env::args_os().skip(1).any(|arg| arg == "--control")
}

/// What the last update did, for the next start to tell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LastUpdate {
    pub(crate) from: String,
    pub(crate) to: String,
    /// Why it failed, when the Windows installer said so.
    #[serde(default)]
    pub(crate) error: Option<String>,
}

pub(crate) fn write_last(updates: &Path, last: &LastUpdate) {
    let written = serde_json::to_vec(last).map_err(std::io::Error::other).and_then(|bytes| {
        std::fs::create_dir_all(updates)?;
        std::fs::write(updates.join(LAST_UPDATE), bytes)
    });
    if let Err(e) = written {
        log::warn!("the update record wasn't written: {e}");
    }
}

/// This process's [`Instance`], held until it exits.
static RUNNING: std::sync::OnceLock<Instance> = std::sync::OnceLock::new();

/// Holds this process's instance locks for as long as it runs, whatever else fails at startup;
/// whether no other copy was running when it started.
pub(crate) fn hold_instance(updates: &Path) -> bool {
    RUNNING.get_or_init(|| Instance::hold(updates)).alone
}

/// This copy's hold on [`INSTANCES`], and on macOS a shared lock on its own executable, which a
/// renamed old bundle keeps: another user's copy can be running from it.
pub(crate) struct Instance {
    _held: Option<std::fs::File>,
    _exe: Option<std::fs::File>,
    /// No other copy was running when this one started.
    pub(crate) alone: bool,
}

impl Instance {
    /// Takes the lock alone when no other copy holds it, then holds it shared like every copy.
    /// Without a lock file (an unwritable folder) this copy never counts as alone.
    pub(crate) fn hold(updates: &Path) -> Self {
        let exe = if cfg!(target_os = "macos") {
            std::env::current_exe().ok().and_then(|exe| std::fs::File::open(exe).ok()).filter(|exe| exe.lock_shared().is_ok())
        } else {
            None
        };
        let (held, alone) = Self::hold_folder(updates);
        Self { _held: held, _exe: exe, alone }
    }

    fn hold_folder(updates: &Path) -> (Option<std::fs::File>, bool) {
        let opened =
            std::fs::create_dir_all(updates).and_then(|()| std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(updates.join(INSTANCES)));
        let file = match opened {
            Ok(file) => file,
            Err(e) => {
                log::info!("update leftovers are kept: no instance lock ({e})");
                return (None, false);
            }
        };
        let alone = match file.try_lock() {
            Ok(()) => file.unlock().is_ok(),
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(e)) => {
                log::info!("update leftovers are kept: the instance lock failed ({e})");
                false
            }
        };
        // Another copy starting now may clean first; it finds nothing of this one's yet.
        if let Err(e) = file.lock_shared() {
            log::info!("the instance lock isn't held ({e})");
            return (None, alone);
        }
        (Some(file), alone)
    }
}

/// Reads and removes the last update's record, and, when no other copy runs (`alone`), removes
/// what earlier updates left behind. Returns what to tell the person.
pub(crate) fn after_start(updates: &Path, kind: Option<&Kind>, alone: bool) -> Option<String> {
    if let Some(kind) = kind.filter(|_| alone) {
        clean(kind, updates);
    }
    let path = updates.join(LAST_UPDATE);
    // A record is a few dozen bytes; anything larger isn't ours.
    let bytes = std::fs::File::open(&path).ok().and_then(|file| {
        let mut bytes = Vec::new();
        file.take(4096).read_to_end(&mut bytes).ok().map(|_| bytes)
    })?;
    let _ = std::fs::remove_file(&path);
    let last: LastUpdate = serde_json::from_slice(&bytes).ok()?;
    if last.to == VERSION {
        return Some(format!("NodeCraft is updated to {VERSION}."));
    }
    (last.from == VERSION).then(|| match last.error {
        Some(error) => format!("The update to NodeCraft {} didn't install: {error}", last.to),
        None => format!("The update to NodeCraft {} didn't install; download it from the releases page.", last.to),
    })
}

/// Removes leftovers of earlier updates beside this copy and in the updates folder.
fn clean(kind: &Kind, updates: &Path) {
    let dir = match kind {
        Kind::MacBundle { bundle, .. } => bundle.parent(),
        Kind::AppImage { file } => file.parent(),
        Kind::WindowsMsi { .. } => Some(updates),
    };
    let Some(entries) = dir.and_then(|dir| std::fs::read_dir(dir).ok()) else { return };
    for entry in entries.flatten().take(10_000) {
        if is_leftover(kind, &entry.file_name().to_string_lossy()) && !running_from(&entry.path()) {
            remove_ours(&entry.path());
        }
    }
}

/// Whether a copy, perhaps another user's, runs from the bundle at `path`: it holds a shared
/// lock on its executable ([`Instance`]).
fn running_from(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path.join("Contents").join("MacOS")) else { return false };
    entries.flatten().take(64).any(|entry| {
        std::fs::File::open(entry.path()).is_ok_and(|exe| match exe.try_lock() {
            Ok(()) => {
                let _ = exe.unlock();
                false
            }
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(_)) => false,
        })
    })
}

/// Whether `name`, beside this copy (or in the updates folder on Windows), is an earlier update's.
fn is_leftover(kind: &Kind, name: &str) -> bool {
    match kind {
        Kind::MacBundle { .. } => name.starts_with(BUNDLE_UPDATE_PREFIX) || name.starts_with(BUNDLE_OLD_PREFIX),
        Kind::AppImage { file } => file.file_name().is_some_and(|own| name.starts_with(&format!(".{}{APPIMAGE_UPDATE_INFIX}", own.to_string_lossy()))),
        Kind::WindowsMsi { .. } => name == HELPER || (name.starts_with("nodecraft-") && name.ends_with(".msi")),
    }
}

/// `--wait-for <pid>`: waits, at most a minute, for the copy that started this one to exit, so
/// this one finds the project and its recovery journal closed.
pub fn wait_for_exit(pid: &str) {
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return;
    }
    let started = std::time::Instant::now();
    while started.elapsed() < WAIT_FOR_EXIT && alive(pid) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn alive(pid: &str) -> bool {
    if cfg!(windows) {
        let filter = format!("PID eq {pid}");
        let out = system("tasklist").args(["/FI", &filter, "/NH", "/FO", "CSV"]).output();
        return out.is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")));
    }
    std::process::Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success())
}

/// The staged MSI, open so that nothing else can change it until it is closed, once it is
/// checked against the signed feed's `blake3`.
fn hold_checked(msi: &Path, blake3: &str) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // Others may read it (msiexec does) but not write, rename or delete it while it is held.
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0x0000_0001); // FILE_SHARE_READ
    let mut file = options.open(msi).map_err(|e| format!("the installer couldn't be opened: {e}"))?;
    nodecraft_engine::update_install::verify_staged(&mut file, blake3, nodecraft_engine::update_install::MAX_PAYLOAD_BYTES).map_err(|error| error.message)?;
    Ok(file)
}

/// `--finish-update <msi> <blake3> <exe> --wait-for <pid> [--control --root/--read-root ...] [project]` (Windows): run by
/// a copy of the app from the updates folder, so the MSI can replace the installed executable.
/// The MSI is run only while it is still the one the feed signed.
pub fn finish_update(msi: &Path, blake3: &str, exe: &Path, pid: &str, project: Option<&Path>, control: bool, roots: (&[PathBuf], &[PathBuf])) {
    let (read, write) = roots;
    wait_for_exit(pid);
    let to = msi.file_stem().map(|stem| stem.to_string_lossy().trim_start_matches("nodecraft-").to_owned()).unwrap_or_default();
    let held = hold_checked(msi, blake3);
    // /passive shows progress and asks for elevation; it never restarts Windows by itself.
    let ran = held.as_ref().map_err(String::clone).map(|_| system("msiexec").arg("/i").arg(msi).args(["/passive", "/norestart"]).status());
    drop(held);
    let error = match ran {
        Err(why) => Some(why),
        Ok(ran) => installer_error(ran),
    };
    if let Some(updates) = msi.parent().filter(|_| error.is_some()) {
        write_last(updates, &LastUpdate { from: VERSION.into(), to, error });
    }
    if let Err(e) = second_hop_command(exe, project, control, read, write).spawn() {
        log::error!("NodeCraft couldn't start after updating: {e}");
    }
}

/// The Windows helper's second hop: recheck after MSI runs, not just before the helper starts.
/// Never forward a grant whose canonical folder disappeared or became a link elsewhere.
fn second_hop_command(exe: &Path, project: Option<&Path>, control: bool, read: &[PathBuf], write: &[PathBuf]) -> std::process::Command {
    let control = if control && check_relaunch_roots(read, write).is_err() {
        log::error!("allowed folders changed during the update; restarting without control access; relaunch manually with the folders you want to allow");
        false
    } else {
        control
    };
    installed_command(exe, project, control, read, write)
}

/// The installed Windows copy's command line after the helper has finished.
fn installed_command(exe: &Path, project: Option<&Path>, control: bool, read: &[PathBuf], write: &[PathBuf]) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    append_grants(&mut command, control, read, write);
    if let Some(project) = project {
        command.arg(project);
    }
    command
}

/// Why `msiexec` didn't install, from how it exited.
fn installer_error(ran: std::io::Result<std::process::ExitStatus>) -> Option<String> {
    match ran {
        Ok(status) if status.success() => None,
        // 1602: the person cancelled; 3010: installed, a restart of Windows finishes it.
        Ok(status) if status.code() == Some(3010) => None,
        Ok(status) if status.code() == Some(1602) => Some("the installer was cancelled".to_owned()),
        Ok(status) => Some(format!("the installer stopped with code {}", status.code().map_or_else(|| "unknown".into(), |code| code.to_string()))),
        Err(e) => Some(format!("the installer couldn't start: {e}")),
    }
}

#[cfg(test)]
mod tests;
