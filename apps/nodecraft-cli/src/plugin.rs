//! `plugin pack|inventory|lint` (#175): building and checking `.ncraft-plugin` packages.
//!
//! `pack` writes a package from a folder, the same bytes for the same files on any machine.
//! `inventory` prints the canonical inventory and digest a package of the folder would have.
//! `lint` checks a package file the way installing, granting and enabling it would. None of them
//! runs anything from the package or the folder, and none touches the app's plugin store.
use std::path::{Path, PathBuf};

use nodecraft_engine::plugin_cmds::{PluginHost, host_ids, lint};
use nodecraft_plugin::inventory::{MAX_FILES, MAX_SEGMENTS};
use nodecraft_plugin_store::ArchiveLimits;
use nodecraft_plugin_store::author::{self, Packed};
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

/// Most bytes read from a folder in all: a package is stored uncompressed, so its files must fit
/// in the archive limit.
const MAX_FOLDER_BYTES: usize = ArchiveLimits::DEFAULT.archive_bytes;
/// Most folders walked: as many as there may be files.
const MAX_FOLDERS: usize = MAX_FILES;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Every file under `folder` as (`/`-separated path, bytes), in path order. Names starting with
/// `.` are left out (`.git`, `.DS_Store`); a link, or a name that isn't UTF-8, is refused rather
/// than followed or guessed at.
pub fn read_folder(folder: &Path) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let mut out = Vec::new();
    let mut total: u64 = 0;
    let mut folders = 0usize;
    let mut pending = vec![(folder.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = pending.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| Error::new(ErrorKind::NotFound, format!("{} can't be read ({e})", dir.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::new(ErrorKind::Internal, format!("{} can't be listed ({e})", dir.display())))?;
            let name = entry.file_name().into_string().map_err(|_| invalid(format!("a name in {} isn't UTF-8", dir.display())))?;
            if name.starts_with('.') {
                continue;
            }
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let kind = entry.file_type().map_err(|e| Error::new(ErrorKind::Internal, format!("{path} can't be read ({e})")))?;
            if kind.is_symlink() {
                return Err(invalid(format!("{path} is a link; a package holds only regular files")));
            } else if kind.is_dir() {
                // A file in this folder would have one more path segment than the folder.
                if depth.saturating_add(2) > MAX_SEGMENTS {
                    return Err(invalid(format!("{path} nests too deep: a package path has at most {MAX_SEGMENTS} segments")));
                }
                folders = folders.saturating_add(1);
                if folders > MAX_FOLDERS {
                    return Err(invalid(format!("the folder holds more than {MAX_FOLDERS} folders")));
                }
                pending.push((entry.path(), path, depth.saturating_add(1)));
            } else if kind.is_file() {
                if out.len() >= MAX_FILES {
                    return Err(invalid(format!("the folder holds more than {MAX_FILES} files")));
                }
                let cap = u64::try_from(MAX_FOLDER_BYTES).unwrap_or(u64::MAX);
                let bytes = read_capped(&entry.path(), cap.saturating_sub(total))
                    .map_err(|e| if e.kind == ErrorKind::InvalidInput { too_big() } else { invalid(format!("{path}: {}", e.message)) })?;
                total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                out.push((path, bytes));
            } else {
                return Err(invalid(format!("{path} isn't a regular file")));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn too_big() -> Error {
    invalid(format!("the folder's files add up to more than {} MiB, the most a package holds", MAX_FOLDER_BYTES / (1024 * 1024)))
}

/// Reads at most `cap` bytes of `path`; a longer file is refused with `InvalidInput`.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| Error::new(ErrorKind::NotFound, format!("it can't be opened ({e})")))?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| Error::new(ErrorKind::Internal, format!("it can't be read ({e})")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
        return Err(invalid(format!("it is larger than {} MiB", cap.div_ceil(1024 * 1024))));
    }
    Ok(bytes)
}

/// `plugin pack <folder> --out <file>`: packs the folder, refusing an output inside it (the next
/// pack would take it in).
pub fn pack(folder: &Path, out: &Path) -> Result<(Packed, Value), Error> {
    let folder_abs = std::fs::canonicalize(folder).map_err(|e| Error::new(ErrorKind::NotFound, format!("{} can't be read ({e})", folder.display())))?;
    let out_dir = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let out_dir = std::fs::canonicalize(out_dir).map_err(|e| Error::new(ErrorKind::NotFound, format!("{} can't be written ({e})", out.display())))?;
    if out_dir.starts_with(&folder_abs) {
        return Err(invalid("write the package outside the folder it is packed from"));
    }
    if std::fs::symlink_metadata(out).is_ok_and(|m| !m.is_file()) {
        return Err(invalid(format!("{} is a link or a folder; name a new or regular file", out.display())));
    }
    let packed = author::pack(read_folder(folder)?, None)?;
    // Written beside the output and moved into place, so a full disk never leaves half a package.
    let partial = out_dir.join(format!(".{}.partial", out.file_name().and_then(|n| n.to_str()).unwrap_or("package")));
    let written = std::fs::write(&partial, &packed.bytes).and_then(|()| std::fs::rename(&partial, out));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::new(ErrorKind::Internal, format!("{} can't be written ({e})", out.display())));
    }
    let report = json!({"file": out.display().to_string(), "digest": packed.digest.to_string(), "bytes": packed.bytes.len()});
    Ok((packed, report))
}

/// `plugin inventory <folder>`: the canonical inventory and digest, without packing.
pub fn inventory(folder: &Path) -> Result<Value, Error> {
    let (inventory, digest) = author::inventory(read_folder(folder)?)?;
    let parsed: Value = serde_json::from_str(&inventory).map_err(|_| Error::new(ErrorKind::Internal, "the inventory couldn't be read back"))?;
    Ok(json!({"digest": digest.to_string(), "inventory": parsed}))
}

/// `plugin lint <file> [--trust <trust.json>]`: checks the package as this NodeCraft would install
/// it, trusting only the keys in the trust file (none without it).
pub fn lint_file(file: &Path, trust: Option<PathBuf>) -> Result<Value, Error> {
    // The store reads a trust file only by absolute path; a named one that is missing is an
    // error here, not "no keys trusted".
    let trust = match trust {
        Some(path) => {
            let path = std::path::absolute(&path).map_err(|e| invalid(format!("the trust file {} can't be found ({e})", path.display())))?;
            if !path.is_file() {
                return Err(Error::new(ErrorKind::NotFound, format!("the trust file {} doesn't exist", path.display())));
            }
            Some(path)
        }
        None => None,
    };
    let limit = u64::try_from(ArchiveLimits::DEFAULT.archive_bytes).unwrap_or(u64::MAX);
    let bytes = read_capped(file, limit).map_err(|e| Error::new(e.kind, format!("{}: {}", file.display(), e.message)))?;
    lint(&bytes, &PluginHost { trust, taken: host_ids()?, bindings: Default::default() })
}

/// What `lint` found, for a person.
pub fn lint_text(report: &Value) -> String {
    let s = |key: &str| report[key].as_str().unwrap_or_default().to_owned();
    let mut text =
        format!("{} {} ({}) passes: it would install.\n  digest {}\n  {}", s("package_id"), s("version"), s("runtime"), s("digest"), s("provenance"));
    if let Some(by) = report["signed_by"].as_str() {
        text.push_str(&format!(", signed by {by}"));
    }
    text.push_str(&format!("; license {}\n", s("license")));
    let list = |key: &str| report[key].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    text.push_str(&format!("  registers: {}\n", list("registrations")));
    let rights = |key: &str| report["rights"][key].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let asks = [("needs", rights("required")), ("may use", rights("optional")), ("reaches", rights("origins"))];
    let asks: Vec<String> = asks.into_iter().filter(|(_, v)| !v.is_empty()).map(|(what, v)| format!("{what} {v}")).collect();
    text.push_str(&format!("  rights: {}\n", if asks.is_empty() { "none asked for".to_owned() } else { asks.join("; ") }));
    for warning in report["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        text.push_str(&format!("  warning: {warning}\n"));
    }
    text
}

/// `plugin <pack|inventory|lint> …`: parses the arguments and prints the result, or the error.
pub fn command(args: &[String]) -> Result<String, String> {
    let (sub, rest) = args.split_first().ok_or("`plugin` needs pack, inventory or lint")?;
    let mut json_out = false;
    let mut out = None;
    let mut trust = None;
    let mut positional = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json_out = true,
            "--out" if sub == "pack" => out = Some(PathBuf::from(it.next().ok_or("--out needs a file")?)),
            "--trust" if sub == "lint" => trust = Some(PathBuf::from(it.next().ok_or("--trust needs a file")?)),
            flag if flag.starts_with("--") => return Err(format!("`plugin {sub}` doesn't take {flag}")),
            path => positional.push(PathBuf::from(path)),
        }
    }
    let [path] = positional.as_slice() else {
        return Err(format!("`plugin {sub}` takes one {}", if sub == "lint" { "package file" } else { "folder" }));
    };
    let shown = |value: &Value| serde_json::to_string_pretty(value).map(|s| s + "\n").map_err(|_| "the result couldn't be written".to_owned());
    match sub.as_str() {
        "pack" => {
            let out = out.ok_or("`plugin pack` needs --out <file>")?;
            let (_, report) = pack(path, &out).map_err(|e| e.message)?;
            if json_out {
                shown(&report)
            } else {
                Ok(format!(
                    "wrote {} ({} bytes)\n  digest {}\n",
                    report["file"].as_str().unwrap_or_default(),
                    report["bytes"],
                    report["digest"].as_str().unwrap_or_default()
                ))
            }
        }
        "inventory" => {
            let report = inventory(path).map_err(|e| e.message)?;
            if json_out {
                shown(&report)
            } else {
                Ok(format!("{}\n{}\n", report["digest"].as_str().unwrap_or_default(), shown(&report["inventory"])?.trim_end()))
            }
        }
        "lint" => {
            let report = lint_file(path, trust).map_err(|e| format!("{} doesn't pass: {}", path.display(), e.message))?;
            if json_out { shown(&report) } else { Ok(lint_text(&report)) }
        }
        other => Err(format!("unknown `plugin` command `{other}`; use pack, inventory or lint")),
    }
}
