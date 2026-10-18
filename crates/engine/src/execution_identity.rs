//! Private launch authority: never serialized or supplied by a caller.
use nodecraft_types::{Error, ErrorKind};

pub(crate) fn nonce() -> Result<[u8; 32], Error> {
    #[cfg(test)]
    if FAIL_ENTROPY.with(std::cell::Cell::get) {
        return Err(Error::new(ErrorKind::Internal, "execution identity entropy is unavailable"));
    }
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "execution identity entropy is unavailable"))?;
    Ok(bytes)
}

pub(crate) struct ExecutionSource {
    nonce: [u8; 32],
    generation: u64,
}
impl ExecutionSource {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self { nonce: nonce()?, generation: 0 })
    }
    pub(crate) fn next(&self) -> Result<u64, Error> {
        self.generation.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "execution source generation exhausted; reopen in a new session"))
    }
    pub(crate) fn advance(&mut self, next: u64) {
        self.generation = next;
    }
    pub(crate) fn bind(&self, hash: &mut blake3::Hasher) {
        hash.update(&self.nonce);
        hash.update(&self.generation.to_le_bytes());
    }
    #[cfg(test)]
    pub(crate) fn exhaust(&mut self) {
        self.generation = u64::MAX;
    }
}

#[cfg(test)]
thread_local! { static FAIL_ENTROPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(crate) fn without_entropy<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAIL_ENTROPY.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(FAIL_ENTROPY.with(|v| v.replace(true)));
    run()
}
