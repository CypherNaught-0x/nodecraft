//! Same-opened-object validation, capacity ownership and reported-body work accounting.
use super::*;
const CHUNK: usize = 64 * 1024;
const INTERRUPTED: usize = 1024;
#[derive(Default)]
struct Work {
    body: usize,
    probes: usize,
    interrupted: usize,
    exhausted: bool,
}
impl Work {
    fn check(&self) -> Result<(), ReadError> {
        if self.exhausted { Err(ReadError::File(Reason::Interrupted)) } else { Ok(()) }
    }
}
fn interrupted(work: &mut Work) -> Result<(), Reason> {
    if work.interrupted >= INTERRUPTED {
        // One refusal-detection call after the tolerated retries. It permanently ends
        // backend I/O for this batch, even though individual file errors are skipped.
        work.exhausted = true;
        return Err(Reason::Interrupted);
    }
    work.interrupted = work.interrupted.checked_add(1).ok_or(Reason::Interrupted)?;
    Ok(())
}
fn body(file: &mut dyn OpenedFile, len: usize, cap: usize, state: &State, work: &mut Work) -> Result<Vec<u8>, ReadError> {
    state.check()?;
    work.check()?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| ReadError::File(Reason::Unreadable))?;
    if bytes.capacity() > cap {
        return Err(ReadError::File(Reason::Aggregate));
    }
    state.check()?;
    bytes.resize(len, 0);
    let mut at = 0usize;
    while at < len {
        state.check()?;
        work.check()?;
        let end = at.checked_add(CHUNK.min(len - at)).ok_or(ReadError::File(Reason::Changed))?;
        let out = bytes.get_mut(at..end).ok_or(ReadError::File(Reason::Changed))?;
        let result = file.read(out);
        // Debit successfully returned bytes even if this call crossed the deadline.
        match result {
            Ok(n) if n > 0 && n <= out.len() => {
                work.body = work.body.checked_add(n).filter(|n| *n <= MAX_DROP_TOTAL_BYTES).ok_or(ReadError::File(Reason::Aggregate))?;
                at = at.checked_add(n).ok_or(ReadError::File(Reason::Changed))?;
            }
            Ok(_) => return Err(ReadError::File(Reason::Changed)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => interrupted(work)?,
            Err(_) => return Err(ReadError::File(Reason::Unreadable)),
        }
        state.check()?;
    }
    let mut probe = [0u8; 1];
    loop {
        state.check()?;
        work.check()?;
        let result = file.read(&mut probe);
        match result {
            Ok(0) => {
                work.probes = work.probes.checked_add(1).ok_or(ReadError::File(Reason::Changed))?;
                state.check()?;
                break;
            }
            Ok(_) => {
                work.probes = work.probes.checked_add(1).ok_or(ReadError::File(Reason::Changed))?;
                return Err(ReadError::File(Reason::Changed));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => interrupted(work)?,
            Err(_) => return Err(ReadError::File(Reason::Unreadable)),
        }
        state.check()?;
    }
    Ok(bytes)
}
enum ReadError {
    File(Reason),
    Whole(Failure),
}
impl From<Reason> for ReadError {
    fn from(e: Reason) -> Self {
        Self::File(e)
    }
}
impl From<Failure> for ReadError {
    fn from(e: Failure) -> Self {
        Self::Whole(e)
    }
}
fn one(shared: &Shared, path: &Path, cap: usize, work: &mut Work, state: &State) -> Result<Vec<u8>, ReadError> {
    state.check()?;
    work.check()?;
    let mut file = shared
        .backend
        .open(path)
        .map_err(|error| ReadError::File(if error.kind() == io::ErrorKind::IsADirectory { Reason::Nonregular } else { Reason::Unreadable }))?;
    state.check()?;
    let (regular, length) = file.metadata().map_err(|_| ReadError::File(Reason::Unreadable))?;
    state.check()?;
    if !regular {
        return Err(ReadError::File(Reason::Nonregular));
    }
    if length > cap as u64 {
        return Err(ReadError::File(if cap < MAX_DROP_BYTES { Reason::Aggregate } else { Reason::TooBig }));
    }
    let len = usize::try_from(length).map_err(|_| ReadError::File(Reason::TooBig))?;
    body(file.as_mut(), len, cap, state, work)
    // Real File closes here, on the worker, on success, error and unwind.
}
pub(super) fn acquire(shared: &Shared, files: Vec<FileRequest>, extra: usize, control: &Control) -> Result<Batch, Failure> {
    let mut read = Vec::new();
    read.try_reserve_exact(files.len()).map_err(|_| Failure::Allocation)?;
    if read.capacity() > MAX_FILES {
        return Err(Failure::Allocation);
    }
    let mut skipped = Summary::default();
    let mut retained = 0usize;
    let mut work = Work::default();
    for request in files {
        control.state.check()?;
        let cap = MAX_DROP_BYTES.min(MAX_DROP_TOTAL_BYTES.saturating_sub(retained)).min(MAX_DROP_TOTAL_BYTES.saturating_sub(work.body));
        let result = match request.source {
            Source::Skip(reason) => Err(ReadError::File(reason)),
            Source::Path(path) if cap == 0 => {
                drop(path);
                Err(ReadError::File(Reason::Aggregate))
            }
            Source::Path(path) => one(shared, &path, cap, &mut work, &control.state),
        };
        control.state.check()?;
        match result {
            Ok(bytes) => {
                retained = retained.checked_add(bytes.capacity()).filter(|n| *n <= MAX_DROP_TOTAL_BYTES).ok_or(Failure::Unexpected)?;
                // The one bounded Vec-to-Arc conversion runs on the reader, never a UI frame.
                read.push(ReadFile { name: request.name, bytes: Arc::from(bytes) });
            }
            Err(ReadError::File(reason)) => skipped.add(Some(request.name), reason, 1)?,
            Err(ReadError::Whole(e)) => return Err(e),
        }
    }
    skipped.add(None, Reason::Many, extra)?;
    control.state.check()?;
    Ok(Batch { files: read, skipped, state: Arc::clone(&control.state), _lease: Arc::clone(&control.lease) })
}
#[cfg(test)]
mod tests;
