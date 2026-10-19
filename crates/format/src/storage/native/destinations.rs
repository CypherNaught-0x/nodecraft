//! Stable private lock inodes are never unlinked. Only active and pending handles live
//! in a storage session; a failed transition cannot release the active destination.
use super::*;

// Upstream's writability probe is named by PID. All this crate's calls must share
// this mutex so independent sessions cannot mistake probe contention for read-only.
static WRITABILITY_PROBE: Mutex<()> = Mutex::new(());

fn project_path(path: &Path) -> Result<PathBuf, Error> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) => match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(io_error("project symlink target cannot be resolved; choose the actual writable destination"))
            }
            // Only an ordinary absent leaf may use the parent/name fallback.
            // In particular, atomic_write follows dangling links, whose actual
            // publication directory would differ from our admission directory.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolved(path),
            _ => Err(io_error("cannot resolve project location; check its permissions and choose the actual destination")),
        },
    }
}

pub(super) struct Destination {
    pub(super) path: PathBuf,
    pub(super) access: ProjectAccess,
    guard: Option<File>,
    admission: Option<File>,
}
pub(super) struct Destinations {
    root: Option<PathBuf>,
    active: Option<Destination>,
    pending: Option<Destination>,
}
impl Destinations {
    pub(super) fn new() -> Self {
        Self {
            root: craft_settings::paths::data_dir("io.github.cyphernaught0x.nodecraft").map(|path| path.join("project-locks-v1")),
            active: None,
            pending: None,
        }
    }
    fn path(&self, location: &StorageLocation, recovery: &Path, lock: &Path) -> Result<PathBuf, Error> {
        let StorageLocation::Path(path) = location else {
            return Err(Error::new(ErrorKind::Unsupported, "native storage requires a filesystem path"));
        };
        let path = project_path(path)?;
        if path == resolved(recovery)? || path == resolved(lock)? {
            return Err(invalid("project location conflicts with session recovery files"));
        }
        if self.root.as_ref().and_then(|root| root.canonicalize().ok()).is_some_and(|root| path.starts_with(root)) {
            return Err(invalid("project location conflicts with private storage locks"));
        }
        Ok(path)
    }
    fn lock_inode(&self, path: &Path, namespace: &str) -> Result<File, Error> {
        let root = self.root.as_ref().ok_or_else(|| io_error("private project lock directory is unavailable"))?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root).map_err(|_| io_error("cannot access private project lock directory"))?;
        let metadata = std::fs::symlink_metadata(root).map_err(|_| io_error("cannot inspect private project lock directory"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io_error("private project lock directory is unsafe"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io_error("private project lock directory permissions are unsafe"));
            }
        }
        let digest = blake3::hash(path.as_os_str().as_encoded_bytes());
        let lock = root.join(format!("{namespace}{digest}.lock"));
        if std::fs::symlink_metadata(&lock).is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink()) {
            return Err(io_error("private project lock file is unsafe"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(lock).map_err(|_| io_error("cannot access private project lock file"))?;
        Ok(file)
    }
    fn acquire(&self, path: &Path) -> Result<Option<File>, Error> {
        if self.active.as_ref().is_some_and(|active| active.path == path && active.guard.is_some()) {
            return Ok(None);
        }
        let file = self.lock_inode(path, "")?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(Error::new(ErrorKind::Busy, "project is already open in another session")),
            Err(_) => Err(io_error("project locking is unavailable")),
        }
    }
    fn admission(&self, path: &Path) -> Result<File, Error> {
        let parent = path.parent().ok_or_else(|| invalid("project needs a parent directory"))?;
        let parent = parent.canonicalize().map_err(|_| io_error("cannot resolve project directory"))?;
        let file = self.lock_inode(&parent, "admission-")?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < Duration::from_secs(2) => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(Error::new(ErrorKind::Busy, "project directory admission is busy; retry")),
                Err(_) => return Err(io_error("project admission locking is unavailable")),
            }
        }
    }
    fn writable(path: &Path) -> bool {
        let Some(parent) = path.parent() else { return false };
        if std::fs::metadata(parent).map_or(true, |metadata| metadata.permissions().readonly())
            || std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
        {
            return false;
        }
        let Ok(_probe) = WRITABILITY_PROBE.lock() else { return false };
        craft_settings::recovery::check_writable(parent).is_ok()
    }
    pub(super) fn prepare(&mut self, location: &StorageLocation, recovery: &Path, lock: &Path) -> Result<(PathBuf, ProjectAccess), Error> {
        self.pending = None;
        let path = self.path(location, recovery, lock)?;
        let admission = match self.admission(&path) {
            Ok(guard) => Some(guard),
            Err(error) if error.kind == ErrorKind::Busy => return Err(error),
            Err(_) => None,
        };
        // Re-resolve only after admission: an absent alias may have been created
        // while this operation was waiting. Opens must name an existing file.
        let path = self.path(location, recovery, lock)?.canonicalize().map_err(|_| io_error("cannot resolve project location"))?;
        let (guard, locked) = match self.acquire(&path) {
            Ok(guard) => (guard, true),
            Err(error) if error.kind == ErrorKind::Busy => return Err(error),
            Err(_) => (None, false),
        };
        let access = if admission.is_some() && locked && Self::writable(&path) { ProjectAccess::ReadWrite } else { ProjectAccess::ReadOnly };
        self.pending = Some(Destination { path: path.clone(), access, guard, admission: None });
        Ok((path, access))
    }
    pub(super) fn save_candidate(&self, location: &StorageLocation, recovery: &Path, lock: &Path) -> Result<Destination, Error> {
        let path = self.path(location, recovery, lock)?;
        let admission = self.admission(&path)?;
        let path = self.path(location, recovery, lock)?;
        if self.active.as_ref().is_some_and(|active| active.path == path && active.access == ProjectAccess::ReadOnly) {
            return Err(io_error("source is read-only; use Save As to a different writable destination"));
        }
        let guard = if path.exists() { self.acquire(&path)? } else { None };
        if !Self::writable(&path) {
            return Err(io_error("destination is read-only; choose a writable Save As destination"));
        }
        Ok(Destination { path, access: ProjectAccess::ReadWrite, guard, admission: Some(admission) })
    }
    pub(super) fn published(&self, destination: Destination) -> Destination {
        self.published_with(destination, |path| path.canonicalize().map_err(|_| io_error("cannot resolve published project")), Self::acquire)
    }
    fn published_with(
        &self,
        mut destination: Destination,
        resolve: impl FnOnce(&Path) -> Result<PathBuf, Error>,
        acquire: impl FnOnce(&Self, &Path) -> Result<Option<File>, Error>,
    ) -> Destination {
        // Publication has happened. No error from here may masquerade as an
        // unpublished save. Admission remains held until adoption below.
        match resolve(&destination.path) {
            Ok(path) => {
                let guard = if destination.guard.is_some() && destination.path == path { Ok(destination.guard.take()) } else { acquire(self, &path) };
                destination.path = path;
                match guard {
                    Ok(guard) => destination.guard = guard,
                    Err(_) => destination.access = ProjectAccess::ReadOnly,
                }
            }
            Err(_) => destination.access = ProjectAccess::ReadOnly,
        }
        destination
    }
    pub(super) fn adopt(&mut self, mut destination: Destination) {
        if destination.guard.is_none()
            && self.active.as_ref().is_some_and(|active| active.path == destination.path)
            && let Some(active) = self.active.as_mut()
        {
            destination.guard = active.guard.take();
        }
        let admission = destination.admission.take();
        self.active = Some(destination);
        drop(admission);
    }
    pub(super) fn commit(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.adopt(pending);
        }
    }
    pub(super) fn cancel(&mut self) {
        self.pending = None;
    }
    pub(super) fn close(&mut self) {
        self.pending = None;
        self.active = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn unresolved_links_are_rejected_before_admission_or_target_publication() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let aliases = dir.path().join("aliases");
        let targets = dir.path().join("targets");
        let root = dir.path().join("private-locks");
        std::fs::create_dir(&aliases).unwrap();
        std::fs::create_dir(&targets).unwrap();
        let recovery = targets.join("recovery");
        let lock = targets.join("recovery.lock");
        let destinations = Destinations { root: Some(root.clone()), active: None, pending: None };
        for (index, target) in [targets.join("new.ncraft"), recovery.clone(), lock.clone(), root.join("reserved.lock")].into_iter().enumerate() {
            let alias = aliases.join(format!("alias-{index}"));
            symlink(&target, &alias).unwrap();
            let error = destinations.save_candidate(&StorageLocation::Path(alias), &recovery, &lock).err().unwrap();
            assert!(error.message.contains("actual writable destination"));
            assert!(!target.exists());
            assert!(!root.exists(), "rejection must precede private admission inode creation");
        }
        let leaf = aliases.join("chain");
        symlink(aliases.join("alias-0"), &leaf).unwrap();
        let cycle_a = aliases.join("cycle-a");
        let cycle_b = aliases.join("cycle-b");
        symlink(&cycle_b, &cycle_a).unwrap();
        symlink(&cycle_a, &cycle_b).unwrap();
        let missing_parent = dir.path().join("missing/leaf.ncraft");
        for path in [leaf, cycle_a, missing_parent] {
            assert!(destinations.save_candidate(&StorageLocation::Path(path), &recovery, &lock).is_err());
            assert!(!root.exists());
        }
        assert!(!targets.join("new.ncraft").exists());
        assert!(destinations.save_candidate(&StorageLocation::Path(targets.join("ordinary-absent.ncraft")), &recovery, &lock).is_ok());
    }
    #[test]
    fn writability_waits_for_the_shared_probe_instead_of_reporting_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("example.ncraft");
        std::fs::write(&path, b"example").unwrap();
        let probe = WRITABILITY_PROBE.lock().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            entered_tx.send(()).unwrap();
            done_tx.send(Destinations::writable(&path)).unwrap();
        });
        entered_rx.recv().unwrap();
        assert!(matches!(done_rx.recv_timeout(Duration::from_millis(30)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)));
        drop(probe);
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
    }
    #[test]
    fn publication_failures_adopt_readonly_and_release_only_after_successful_publication() {
        for fail_identity in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut owner = Destinations { root: Some(dir.path().join("locks")), active: None, pending: None };
            let recovery = dir.path().join("recovery");
            let lock = dir.path().join("recovery.lock");
            let old = StorageLocation::Path(dir.path().join("old.ncraft"));
            let next = StorageLocation::Path(dir.path().join("next.ncraft"));
            let candidate = owner.save_candidate(&old, &recovery, &lock).unwrap();
            std::fs::write(&candidate.path, b"old").unwrap();
            owner.adopt(owner.published(candidate));
            let candidate = owner.save_candidate(&next, &recovery, &lock).unwrap();
            // Deterministic protocol proof: gate remains locked across publication.
            let gate = owner.lock_inode(candidate.path.parent().unwrap(), "admission-").unwrap();
            assert!(matches!(gate.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
            let old_path = owner.active.as_ref().unwrap().path.clone();
            let contender = Destinations { root: owner.root.clone(), active: None, pending: None };
            assert_eq!(contender.acquire(&old_path).unwrap_err().kind, ErrorKind::Busy);
            std::fs::write(&candidate.path, b"published").unwrap();
            let candidate = owner.published_with(
                candidate,
                |path| if fail_identity { Err(io_error("injected identity failure")) } else { path.canonicalize().map_err(|_| io_error("identity")) },
                |_, _| Err(io_error("injected project guard failure")),
            );
            assert_eq!(candidate.access, ProjectAccess::ReadOnly);
            assert_eq!(contender.acquire(&old_path).unwrap_err().kind, ErrorKind::Busy);
            owner.adopt(candidate);
            gate.try_lock().unwrap();
            drop(gate);
            assert!(contender.acquire(&old_path).is_ok());
            assert_eq!(std::fs::read(dir.path().join("next.ncraft")).unwrap(), b"published");
            assert!(owner.save_candidate(&next, &recovery, &lock).is_err());
            assert!(owner.save_candidate(&StorageLocation::Path(dir.path().join("independent.ncraft")), &recovery, &lock).is_ok());
        }
    }
    #[test]
    fn unavailable_lock_infrastructure_is_readonly_and_never_admits_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("example.ncraft");
        std::fs::write(&path, b"example").unwrap();
        let location = StorageLocation::Path(path);
        let recovery = dir.path().join("recovery");
        let lock = dir.path().join("recovery.lock");
        let mut destinations = Destinations { root: None, active: None, pending: None };
        let (_, access) = destinations.prepare(&location, &recovery, &lock).unwrap();
        assert_eq!(access, ProjectAccess::ReadOnly);
        destinations.commit();
        assert!(destinations.save_candidate(&location, &recovery, &lock).is_err());
        assert!(destinations.save_candidate(&StorageLocation::Path(dir.path().join("new.ncraft")), &recovery, &lock).is_err());
    }

    #[test]
    fn private_lock_inode_persists_across_release_and_cannot_be_a_save_destination() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("locks");
        let location = StorageLocation::Path(dir.path().join("example.ncraft"));
        let recovery = dir.path().join("recovery");
        let lock = dir.path().join("recovery.lock");
        let mut first = Destinations { root: Some(root.clone()), active: None, pending: None };
        let candidate = first.save_candidate(&location, &recovery, &lock).unwrap();
        std::fs::write(&candidate.path, b"example").unwrap();
        let candidate = first.published(candidate);
        first.adopt(candidate);
        let inode = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| !path.file_name().unwrap().to_str().unwrap().starts_with("admission-"))
            .unwrap();
        #[cfg(unix)]
        let original_inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&inode).unwrap().ino()
        };
        let second = Destinations { root: Some(root), active: None, pending: None };
        assert!(first.save_candidate(&StorageLocation::Path(inode.clone()), &recovery, &lock).is_err());
        assert_eq!(second.save_candidate(&location, &recovery, &lock).err().unwrap().kind, ErrorKind::Busy);
        first.close();
        assert!(inode.exists());
        let candidate = second.save_candidate(&location, &recovery, &lock).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&inode).unwrap().ino(), original_inode);
        }
        drop(candidate);
    }
}
