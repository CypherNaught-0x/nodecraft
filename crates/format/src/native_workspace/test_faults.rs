//! Per-thread, owned-root fault injection; no production state or global environment.
use super::*;
use std::cell::RefCell;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Point {
    PayloadUnlink,
    PayloadChanged,
    MarkerUnlink,
    DirectoryRetire,
    NewMarker,
}
struct Fault {
    root: PathBuf,
    point: Point,
    skip: usize,
    hits: usize,
}
thread_local! { static FAULT: RefCell<Option<Fault>> = const { RefCell::new(None) }; }
pub(super) struct Guard {
    previous: Option<Fault>,
}
impl Guard {
    pub(super) fn arm(root: &Path, point: Point, skip: usize) -> Self {
        let previous = FAULT.with(|state| state.replace(Some(Fault { root: root.canonicalize().unwrap(), point, skip, hits: 0 })));
        Self { previous }
    }
    pub(super) fn hits(&self) -> usize {
        FAULT.with(|state| state.borrow().as_ref().unwrap().hits)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        FAULT.with(|state| state.replace(self.previous.take()));
    }
}
fn hit(point: Point, path: &Path) -> bool {
    FAULT.with(|state| {
        let mut state = state.borrow_mut();
        let Some(fault) = state.as_mut() else { return false };
        // New-marker failures target the exact owned parent, before its fresh root is known.
        let matching = if point == Point::NewMarker {
            path.parent().and_then(Path::parent) == Some(fault.root.as_path())
        } else {
            path == fault.root || path.parent() == Some(fault.root.as_path())
        };
        if point != fault.point || !matching {
            return false;
        }
        if fault.skip > 0 {
            fault.skip -= 1;
            return false;
        }
        fault.hits += 1;
        true
    })
}
pub(super) fn check(point: Point, path: &Path) -> std::io::Result<()> {
    if hit(point, path) { Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)) } else { Ok(()) }
}
pub(super) fn before_payload(path: &Path) {
    if hit(Point::PayloadChanged, path) && path.is_file() {
        std::fs::remove_file(path).unwrap();
        std::fs::create_dir(path).unwrap();
    }
}
