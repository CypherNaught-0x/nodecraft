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
//! Every new process is started with `--wait-for <pid>`, so it opens the project only after this
//! one has closed it. What happened is written to `last-update.json` in the updates folder, and
//! the next start says so. Leftovers (the renamed old bundle, a half-staged update, the helper)
//! are removed at the next start; only names this module makes are ever removed.
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
        return match std::env::var_os("APPIMAGE").map(PathBuf::from) {
            Some(file) if file.is_absolute() && file.is_file() => Ok(Kind::AppImage { file }),
            _ => Err("only the AppImage updates itself; update this copy the way it was installed".into()),
        };
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

/// Where the MSI installed NodeCraft (packaging/windows/nodecraft.wxs records it).
fn msi_install_dir() -> Option<PathBuf> {
    let out = std::process::Command::new("reg").args(["query", r"HKLM\Software\NodeCraft", "/v", "InstallDir"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    reg_value(&text).map(PathBuf::from)
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
}

/// What to start once this process has closed the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Relaunch {
    Bundle(PathBuf),
    Exec(PathBuf),
    /// The helper copy, the MSI and the executable it starts afterwards.
    Helper {
        helper: PathBuf,
        msi: PathBuf,
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
    let path = match kind {
        Kind::MacBundle { bundle, verify } => {
            let parent = bundle.parent().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the app has no folder to update in"))?;
            let staged = parent.join(format!("{BUNDLE_UPDATE_PREFIX}{version}.app"));
            remove_ours(&staged);
            if let Err(error) = unzip_bundle(&downloaded.bytes, &staged) {
                remove_ours(&staged);
                return Err(error);
            }
            if *verify && let Err(error) = verify_signature(&staged) {
                remove_ours(&staged);
                return Err(error);
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
    Ok(Staged { version: version.clone(), kind: kind.clone(), path })
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

/// Swaps the staged update in. `updates` holds the Windows helper. On an error nothing has
/// changed: the running copy is where it was.
pub(crate) fn apply(staged: &Staged, updates: &Path) -> Result<Relaunch, Error> {
    let relaunch = match &staged.kind {
        Kind::MacBundle { bundle, .. } => {
            let parent = bundle.parent().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the app has no folder"))?;
            let old = parent.join(format!("{BUNDLE_OLD_PREFIX}{VERSION}.app"));
            remove_ours(&old);
            std::fs::rename(bundle, &old).map_err(|e| io("moving aside", bundle, &e))?;
            if let Err(e) = std::fs::rename(&staged.path, bundle) {
                // Put the running copy back before saying anything.
                if let Err(back) = std::fs::rename(&old, bundle) {
                    log::error!("NodeCraft couldn't move {} back to {}: {back}", old.display(), bundle.display());
                }
                return Err(io("moving in", bundle, &e));
            }
            Relaunch::Bundle(bundle.clone())
        }
        Kind::AppImage { file } => {
            std::fs::rename(&staged.path, file).map_err(|e| io("replacing", file, &e))?;
            Relaunch::Exec(file.clone())
        }
        Kind::WindowsMsi { exe } => {
            let helper = updates.join(HELPER);
            remove_ours(&helper);
            std::fs::copy(exe, &helper).map_err(|e| io("copying", exe, &e))?;
            Relaunch::Helper { helper, msi: staged.path.clone(), exe: exe.clone() }
        }
    };
    Ok(relaunch)
}

/// Starts the new copy (or the Windows helper); it waits for this process to exit.
pub(crate) fn relaunch(relaunch: &Relaunch, project: Option<&Path>) -> Result<(), Error> {
    let pid = std::process::id().to_string();
    let mut command = match relaunch {
        Relaunch::Bundle(bundle) => {
            let mut command = std::process::Command::new("/usr/bin/open");
            command.arg("-n").arg(bundle).arg("--args");
            command
        }
        Relaunch::Exec(file) => std::process::Command::new(file),
        Relaunch::Helper { helper, msi, exe } => {
            let mut command = std::process::Command::new(helper);
            command.arg("--finish-update").arg(msi).arg(exe);
            command
        }
    };
    command.args(["--wait-for", &pid]);
    if let Some(project) = project {
        command.arg(project);
    }
    command.spawn().map(drop).map_err(|e| Error::new(ErrorKind::Internal, format!("NodeCraft couldn't start again after updating: {e}; open it yourself")))
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

/// Reads and removes the last update's record, and removes what that update left behind.
/// Returns what to tell the person.
pub(crate) fn after_start(updates: &Path, kind: Option<&Kind>) -> Option<String> {
    if let Some(kind) = kind {
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
        if is_leftover(kind, &entry.file_name().to_string_lossy()) {
            remove_ours(&entry.path());
        }
    }
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
        let out = std::process::Command::new("tasklist").args(["/FI", &filter, "/NH", "/FO", "CSV"]).output();
        return out.is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")));
    }
    std::process::Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success())
}

/// `--finish-update <msi> <exe> --wait-for <pid> [project]` (Windows): run by a copy of the app
/// from the updates folder, so the MSI can replace the installed executable.
pub fn finish_update(msi: &Path, exe: &Path, pid: &str, project: Option<&Path>) {
    wait_for_exit(pid);
    let updates = msi.parent().map(Path::to_path_buf).unwrap_or_default();
    let to = msi.file_stem().map(|stem| stem.to_string_lossy().trim_start_matches("nodecraft-").to_owned()).unwrap_or_default();
    // /passive shows progress and asks for elevation; it never restarts Windows by itself.
    let error = match std::process::Command::new("msiexec").arg("/i").arg(msi).args(["/passive", "/norestart"]).status() {
        Ok(status) if status.success() => None,
        // 1602: the person cancelled; 3010: installed, a restart of Windows finishes it.
        Ok(status) if status.code() == Some(3010) => None,
        Ok(status) if status.code() == Some(1602) => Some("the installer was cancelled".to_owned()),
        Ok(status) => Some(format!("the installer stopped with code {}", status.code().map_or_else(|| "unknown".into(), |code| code.to_string()))),
        Err(e) => Some(format!("the installer couldn't start: {e}")),
    };
    if error.is_some() {
        write_last(&updates, &LastUpdate { from: VERSION.into(), to, error });
    }
    let mut command = std::process::Command::new(exe);
    if let Some(project) = project {
        command.arg(project);
    }
    if let Err(e) = command.spawn() {
        log::error!("NodeCraft couldn't start after updating: {e}");
    }
}

#[cfg(test)]
mod tests;
