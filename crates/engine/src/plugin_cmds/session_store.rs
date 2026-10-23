//! The `plugins` service where packages last only for the session (the browser build, #823).
//! There is no path to read: the person picks a file, its bytes are held as [`PickedPackage`],
//! and `plugins.import` / `plugins.update` name the location it was given. Lists say the store
//! is session-only, and nothing in it may run: only data packages can be enabled.
use super::*;
use nodecraft_plugin::manifest::Runtime;
use std::cell::RefCell;
use std::rc::Rc;

/// Most bytes of a picked file's name kept in its location.
const MAX_NAME_BYTES: usize = 255;

#[derive(Default)]
struct Slot {
    picks: u64,
    file: Option<(String, Rc<Vec<u8>>)>,
}

/// The package file the person picked last, shared by the panel and the service.
#[derive(Clone, Default)]
pub struct PickedPackage(Rc<RefCell<Slot>>);

impl PickedPackage {
    /// Holds `bytes`, picked as `name`, in place of the file picked before; the location
    /// `plugins.import` names it by. A file over the package limit is refused.
    pub fn put(&self, name: &str, bytes: Vec<u8>) -> Result<String, Error> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > package_limit() {
            return Err(Error::new(ErrorKind::InvalidInput, "the package file is too large"));
        }
        let mut slot = self.0.try_borrow_mut().map_err(|_| busy())?;
        slot.picks = slot.picks.saturating_add(1);
        let mut short = String::new();
        for c in name.chars().filter(|c| !c.is_control() && *c != '/') {
            if short.len().saturating_add(c.len_utf8()) > MAX_NAME_BYTES {
                break;
            }
            short.push(c);
        }
        let location = format!("picked/{}/{}", slot.picks, if short.is_empty() { "package" } else { &short });
        slot.file = Some((location.clone(), Rc::new(bytes)));
        Ok(location)
    }

    /// Lets go of the picked file; the store keeps its own copy of what was installed.
    pub fn clear(&self) {
        if let Ok(mut slot) = self.0.try_borrow_mut() {
            slot.file = None;
        }
    }

    fn read(&self, location: &str) -> Result<Rc<Vec<u8>>, Error> {
        let slot = self.0.try_borrow().map_err(|_| busy())?;
        match &slot.file {
            Some((held, bytes)) if held == location => Ok(Rc::clone(bytes)),
            _ => Err(Error::new(ErrorKind::NotFound, "that package file isn't picked any more; choose it again")),
        }
    }
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the picked package file is in use; try again")
}

fn cant_run() -> Error {
    Error::new(ErrorKind::Unsupported, "plugins can't run in the browser yet; only packages that carry data can be enabled here")
}

/// Registers the `plugins` service over a session-only `store`, reading package files from the
/// returned [`PickedPackage`].
pub fn register_session_plugins<B: StoreBackend + 'static>(services: &mut Services, mut store: Store<B>, host: PluginHost) -> Result<PickedPackage, Error> {
    let picked = PickedPackage::default();
    let files = picked.clone();
    services.register(PLUGIN_SERVICE, move |request: &Value| {
        let op = request.get("op").and_then(Value::as_str);
        match op {
            Some("grant") => return Err(cant_run()),
            Some("enable") => {
                store.refresh()?;
                if !matches!(store.load(&package_id(request)?)?.manifest.runtime, Runtime::Data) {
                    return Err(cant_run());
                }
            }
            _ => {}
        }
        let mut out = handle_reading(&mut store, &host, request, &mut |location| files.read(location))?;
        let installing = matches!(op, Some("import" | "update"));
        if installing && request.get("dry_run").and_then(Value::as_bool) != Some(true) {
            // Up to 32 MiB the store already holds a copy of; wasm memory never shrinks back.
            files.clear();
        }
        if (installing || op == Some("list"))
            && let Some(o) = out.as_object_mut()
        {
            o.insert("session_only".into(), json!(true));
        }
        Ok(out)
    })?;
    Ok(picked)
}
