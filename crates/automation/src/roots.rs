//! The folders automation may open projects from and save them to (#423).
//!
//! An agent reaches `file.open`, `file.save_as`, `file.save` and `file.cancel` only on a host the
//! person started with allowed folders (`nodecraft --control --root <folder>`). A path is checked
//! after the OS resolves it (`..`, symbolic links), and the command gets the resolved path, so a
//! link inside a root can't lead outside it when it is checked. Something else that can change the
//! folders in between (a link swapped in after the check) isn't guarded against. Without folders the commands are refused with what
//! to do instead; a native host's own callers (menus, shortcuts) are never checked here.
use std::path::{Component, Path, PathBuf};

use nodecraft_types::{Error, ErrorKind};
use serde_json::{Map, Value};

use crate::commands::CommandHost;

/// Most allowed folders a host takes.
pub const MAX_ROOTS: usize = 32;

/// The commands that read or write project files, which only allowed folders admit.
pub const FILE_COMMANDS: &[&str] = &["file.open", "file.save_as", "file.save", "file.cancel"];

/// Allowed folders, resolved when the host starts. A `write` folder can be read as well.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileRoots {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

impl FileRoots {
    /// Resolves each folder, which must exist; a missing one is an error, never "allow nothing".
    pub fn new(read: &[PathBuf], write: &[PathBuf]) -> Result<Self, Error> {
        if read.len().saturating_add(write.len()) > MAX_ROOTS {
            return Err(invalid(format!("at most {MAX_ROOTS} allowed folders")));
        }
        let resolve = |dir: &PathBuf| -> Result<PathBuf, Error> {
            let resolved = std::fs::canonicalize(dir).map_err(|e| invalid(format!("the allowed folder {} can't be used ({e})", dir.display())))?;
            if !resolved.is_dir() {
                return Err(invalid(format!("the allowed folder {} isn't a folder", dir.display())));
            }
            Ok(resolved)
        };
        Ok(Self { read: read.iter().map(resolve).collect::<Result<_, _>>()?, write: write.iter().map(resolve).collect::<Result<_, _>>()? })
    }

    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.write.is_empty()
    }

    /// Canonical folders granted read access at startup, for an update relaunch.
    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read
    }

    /// Canonical folders granted write access at startup, for an update relaunch.
    pub fn write_roots(&self) -> &[PathBuf] {
        &self.write
    }

    fn readable(&self, path: &Path) -> bool {
        self.read.iter().chain(&self.write).any(|root| path.starts_with(root))
    }

    fn writable(&self, path: &Path) -> bool {
        self.write.iter().any(|root| path.starts_with(root))
    }

    fn listed(roots: &[PathBuf]) -> String {
        if roots.is_empty() { "none".to_owned() } else { roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ") }
    }

    /// The resolved path of an existing project file inside an allowed folder.
    pub fn open_path(&self, location: &str) -> Result<PathBuf, Error> {
        let path = absolute(location)?;
        let resolved = std::fs::canonicalize(path).map_err(|_| Error::new(ErrorKind::NotFound, "no project file at that location"))?;
        if !resolved.is_file() {
            return Err(invalid("that location isn't a file"));
        }
        if !self.readable(&resolved) {
            return Err(Error::new(
                ErrorKind::Forbidden,
                format!("that file isn't in a folder automation may open from: {}", Self::listed(&[self.read.clone(), self.write.clone()].concat())),
            ));
        }
        Ok(resolved)
    }

    /// Where saving to `location` writes: its folder resolved, joined with its plain file name.
    /// A file already there is resolved too, so a link can't point the write outside.
    pub fn save_path(&self, location: &str) -> Result<PathBuf, Error> {
        let path = absolute(location)?;
        let (Some(parent), Some(Component::Normal(name))) = (path.parent(), path.components().next_back()) else {
            return Err(invalid("location must name a file"));
        };
        let folder = std::fs::canonicalize(parent).map_err(|_| Error::new(ErrorKind::NotFound, "the folder to save in doesn't exist"))?;
        let target = folder.join(name);
        let resolved = match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_dir() => return Err(invalid("that location is a folder")),
            Ok(_) => std::fs::canonicalize(&target).map_err(|_| invalid("that location can't be resolved"))?,
            Err(_) => target,
        };
        self.check_writable(&resolved)?;
        Ok(resolved)
    }

    fn check_writable(&self, path: &Path) -> Result<(), Error> {
        if self.writable(path) {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Forbidden, format!("that file isn't in a folder automation may save to: {}", Self::listed(&self.write))))
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn absolute(location: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(location);
    if location.is_empty() || location.contains('\0') || !path.is_absolute() {
        return Err(invalid("location must be an absolute path"));
    }
    Ok(path)
}

fn text(path: &Path) -> Result<String, Error> {
    path.to_str().map(str::to_owned).ok_or_else(|| invalid("the resolved location isn't UTF-8"))
}

/// `params` for `id` once its files are inside the host's allowed folders: a file command gets
/// the resolved path. Other commands pass unchanged.
pub fn admit<H: CommandHost + ?Sized>(host: &H, id: &str, params: Value) -> Result<Value, Error> {
    if !FILE_COMMANDS.contains(&id) {
        return Ok(params);
    }
    let Some(roots) = host.file_roots().filter(|r| !r.is_empty()) else {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("{id} needs folders automation may use: start NodeCraft with `--control --root <folder>` and connect with `nodecraft-cli mcp --bridge`"),
        ));
    };
    let mut object = match params {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        _ => return Err(invalid(format!("{id} takes an object of params"))),
    };
    match id {
        "file.open" => {
            if object.get("discard_dirty").and_then(Value::as_bool) == Some(true) {
                return Err(Error::new(ErrorKind::Conflict, "automation can't discard unsaved changes; save them first with file.save or file.save_as"));
            }
            let location = object.get("location").and_then(Value::as_str).ok_or_else(|| invalid("file.open needs `location`"))?;
            let resolved = roots.open_path(location)?;
            object.insert("location".into(), Value::String(text(&resolved)?));
        }
        "file.save_as" => {
            let location = object.get("location").and_then(Value::as_str).ok_or_else(|| invalid("file.save_as needs `location`"))?;
            let resolved = roots.save_path(location)?;
            object.insert("location".into(), Value::String(text(&resolved)?));
        }
        "file.save" => match host.session().file_status().location {
            Some(nodecraft_format::StorageLocation::Path(current)) => {
                // Resolved like a new save, so a project file that is gone (or a `..` in how it was
                // opened) is judged by where the write would really land.
                let current = text(&current)?;
                roots.save_path(&current)?;
            }
            Some(nodecraft_format::StorageLocation::Key(_)) => return Err(Error::new(ErrorKind::Unsupported, "automation saves only to files")),
            // The engine says what to do with an untitled project.
            None => {}
        },
        _ => {}
    }
    Ok(Value::Object(object))
}

#[cfg(test)]
mod tests;
